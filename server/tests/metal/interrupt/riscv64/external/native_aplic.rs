use kernel::metal::interrupt::treaty::{AbsoluteTimer, Completion, HardwareClaim};
use kernel::metal::platform::boot::treaty as boot_diagnostic;

#[path = "../../claim.rs"]
mod claim_consumer;
#[path = "../../completion.rs"]
mod consumer;

use kernel::{
    entry::boot::Boot,
    metal::{
        chrono::riscv64::{description::RiscvTimebase, time::Counter},
        interrupt::riscv64::{
            external::{
                Barrier,
                aplic::{self, Domain, Mode, Source},
            },
            internal::{
                aclint,
                supervisor::Reception,
                timer::{self, State, Timer},
            },
        },
        memory::access::treaty::device::DeviceWindow,
    },
};
use limen_treaty::model::InternalAddress;

const INTERRUPT: usize = 1 << 63;
const TIMER: usize = INTERRUPT | 5;
const EXTERNAL: usize = INTERRUPT | 9;

#[path = "../window.rs"]
mod reception;
use reception::receive;

fn timer_test(boot: &Boot<'_>, local: &mut Reception, wrong: &mut Reception) {
    let tree = boot.device_tree_description().expect("trusted RV64 DTB");
    let cpus = boot.riscv_cpus();
    let timebase = RiscvTimebase::describe(cpus).unwrap().unwrap();
    let description = aclint::Description::read(&tree, cpus, boot.processor() as u64)
        .unwrap()
        .expect("ACLINT machine timer");
    assert_eq!(description.clock_scope(), timebase.source());
    assert_eq!(description.counter().length().get(), 8);
    assert_eq!(description.comparator().length().get(), 8);
    assert_eq!(description.hart(), boot.processor() as u64);
    // The runner's exact machine configuration has ACLINT and disables Sstc.
    // OpenSBI owns the machine comparator; the guest never maps or writes it.
    let service = boot.sbi_services().timer().unwrap().expect("SBI TIME");
    assert!(unsafe { Counter::admit(timebase, u64::MAX) }.is_none());
    let counter = unsafe { Counter::admit(timebase, boot.processor() as u64) }.unwrap();
    assert!(matches!(
        unsafe { Timer::admit_sbi(&counter, service, wrong) },
        Err(timer::Failure::Scope)
    ));
    let mut timer = unsafe { Timer::admit_sbi(&counter, service, local) }.unwrap();
    assert_eq!(timer.state(), State::Inherited(None));
    assert_eq!(timer.arm_at(counter.deadline(1)), Err(timer::Failure::Busy));
    assert_eq!(
        unsafe { timer.rearm(counter.deadline(1)) },
        Err(timer::Failure::Busy)
    );
    assert_eq!(timer.due(), Err(timer::Failure::Busy));
    assert_eq!(timer.state(), State::Inherited(None));
    unsafe { timer.clear() }.unwrap();
    assert_eq!(timer.state(), State::Idle);
    assert!(!timer.pending().unwrap());
    let hertz = u64::from(timebase.hertz());
    let now = timer.counter().unwrap();
    let separate = unsafe { Counter::admit(timebase, boot.processor() as u64) }.unwrap();
    assert_eq!(
        timer.arm_at(separate.deadline(now + hertz)),
        Err(timer::Failure::Scope)
    );
    assert_eq!(
        timer.arm_at(counter.deadline(now)),
        Err(timer::Failure::Range)
    );
    assert_eq!(
        timer.arm_at(counter.deadline(u64::MAX)),
        Err(timer::Failure::Range)
    );
    assert_eq!(
        unsafe { timer.rearm(counter.deadline(now + hertz)) },
        Err(timer::Failure::Busy)
    );
    assert_eq!(timer.state(), State::Idle);
    for round in 0..2 {
        let now = timer.counter().unwrap();
        let target = now.checked_add(hertz / 20).unwrap();
        let limit = now.checked_add(hertz).unwrap();
        if round == 0 {
            let later = limit.checked_add(hertz).unwrap();
            timer.arm_at(counter.deadline(later)).unwrap();
            assert_eq!(timer.state(), State::Armed(later));
            assert!(!timer.due().unwrap());
            assert!(matches!(
                timer.arm_at(counter.deadline(target)),
                Err(timer::Failure::Busy)
            ));
            assert_eq!(
                unsafe { timer.rearm(counter.deadline(now)) },
                Err(timer::Failure::Range)
            );
            assert_eq!(timer.state(), State::Armed(later));
        }
        if round == 0 {
            timer.advance_deadline(counter.deadline(target)).unwrap();
        } else {
            // The received first obligation is settled. The native replacement
            // also clears its pending bit without an intervening clear.
            unsafe { timer.rearm(counter.deadline(target)) }.unwrap();
        }
        assert!(timer.pending_effect().unwrap().is_none());
        assert_eq!(timer.state(), State::Armed(target));
        assert!(!timer.pending().unwrap());
        assert_eq!(unsafe { receive(limit) }, TIMER, "supervisor timer trap");
        assert!(timer.pending().unwrap());
        assert!(timer.due().unwrap());
    }
    unsafe { timer.clear() }.unwrap();
    assert_eq!(timer.state(), State::Idle);
    assert!(!timer.pending().unwrap());
    assert_eq!(timer.due(), Err(timer::Failure::Busy));
    unsafe { timer.clear() }.unwrap();
    assert_eq!(timer.state(), State::Idle);
    assert!(timer.pending_effect().unwrap().is_none());
    assert!(timer.confirm_effect().is_err());
    boot_diagnostic::write("PASS sbi-timer received=2 cleared=2 states=1 rejected=1\n");
}

fn uart_read(window: &mut DeviceWindow<'_>, register: usize) -> u8 {
    // The test owns the QEMU ns16550a byte register window during this step.
    unsafe { window.read_u8(register) }.unwrap()
}

fn uart_write(window: &mut DeviceWindow<'_>, register: usize, value: u8) {
    unsafe {
        window.write_u8(register, value).unwrap();
        core::arch::asm!("fence iorw, iorw", options(nostack));
    }
}

fn external_test(boot: &Boot<'_>, local: &mut Reception, wrong: &mut Reception) {
    let tree = boot.device_tree_description().unwrap();
    let cpus = boot.riscv_cpus();
    let timebase = RiscvTimebase::describe(cpus).unwrap().unwrap();
    let description = aplic::Description::read(&tree, cpus, boot.processor() as u64)
        .unwrap()
        .expect("supervisor direct APLIC");
    let domain_description = description.domain();
    let destination = description.destination();
    assert_eq!(destination.contexts(), 1);
    assert_eq!(destination.index(), 0);
    let uart = tree
        .find_path(b"/soc/serial@10000000")
        .unwrap()
        .expect("QEMU UART");
    assert!(uart.compatible(b"ns16550a").unwrap());
    let range = uart.register(0).unwrap();
    assert!(range.length().get() >= 8);
    // Bare QEMU virt supplies this exact device mapping; the fixture owns
    // the register accesses, and uses no firmware console during reception.
    let mut serial =
        unsafe { DeviceWindow::admit(range, InternalAddress::new(range.start().get())) }.unwrap();
    let route = domain_description.route(&tree, uart).unwrap();
    assert_eq!(route.mode(), Mode::LevelHigh);
    assert_eq!(uart_read(&mut serial, 3) & 0x80, 0, "DLAB must be clear");
    assert_eq!(
        uart_read(&mut serial, 1),
        0,
        "firmware console must be polling"
    );
    let base = InternalAddress::new(domain_description.registers().start().get());
    // The single-Hart QEMU board, initial bare mapping, quiet device and
    // supervisor domain delegated by OpenSBI establish this test's ownership.
    let counter = unsafe { Counter::admit(timebase, boot.processor() as u64) }.unwrap();
    let mut mapping = unsafe { DeviceWindow::admit(domain_description.registers(), base) }.unwrap();
    assert_eq!(mapping.physical(), domain_description.registers());
    assert!(mapping.supports_u32(0));
    assert!(!mapping.supports_u32(1));
    let end = domain_description.registers().length().as_usize().unwrap();
    assert!(!mapping.supports_u32(end));
    assert!(!mapping.supports_u32(usize::MAX));
    // All of these requests fail before an actual bus access.
    use kernel::metal::memory::access::treaty::device::Error as DeviceError;
    assert_eq!(unsafe { mapping.read_u32(1) }, Err(DeviceError::Range));
    assert_eq!(
        unsafe { mapping.write_u32(end, 0) },
        Err(DeviceError::Range)
    );
    assert_eq!(
        unsafe { mapping.read_u32(usize::MAX) },
        Err(DeviceError::Range)
    );
    let serial_end = range.length().as_usize().unwrap();
    assert_eq!(
        unsafe { serial.read_u8(serial_end) },
        Err(DeviceError::Range)
    );
    assert_eq!(
        unsafe { serial.write_u8(usize::MAX, 0) },
        Err(DeviceError::Range)
    );
    let mut storage = [Source::Vacant; 1023];
    let storage = &mut storage[..domain_description.sources() as usize];
    assert!(matches!(
        unsafe { Domain::admit(description, &mut mapping, &mut [], local, Barrier::native()) },
        Err(aplic::Failure::Range)
    ));
    assert!(matches!(
        unsafe { Domain::admit(description, &mut mapping, storage, wrong, Barrier::native()) },
        Err(aplic::Failure::Scope)
    ));
    let (mut domain, mut context) =
        unsafe { Domain::admit(description, &mut mapping, storage, local, Barrier::native()) }
            .unwrap();
    assert_eq!(context.destination(), destination);
    assert_eq!(core::mem::size_of::<aplic::Source>(), 2);
    assert_eq!(domain.state(), aplic::State::Prepared);
    assert_eq!(domain.source(route.id()).unwrap(), aplic::Source::Vacant);
    assert_eq!(domain.source(0), Err(aplic::Failure::Range));
    unsafe { domain.configure(&mut context, route) }.unwrap();
    assert_eq!(
        domain.source(route.id()).unwrap(),
        aplic::Source::Masked(route.mode())
    );
    assert_eq!(
        domain.enable(&mut context, route.id()),
        Err(aplic::Failure::Busy)
    );
    assert!(matches!(
        domain.receiver(&mut context).unwrap().claim(),
        Err(aplic::Failure::Busy)
    ));
    assert_eq!(
        unsafe { domain.configure(&mut context, route) },
        Err(aplic::Failure::Busy)
    );
    unsafe { domain.open(&mut context) }.unwrap();
    assert_eq!(domain.state(), aplic::State::Open);
    assert_eq!(
        unsafe { domain.open(&mut context) },
        Err(aplic::Failure::Busy)
    );
    assert!(
        domain
            .receiver(&mut context)
            .unwrap()
            .claim()
            .unwrap()
            .is_none()
    );
    let mut previous = 0;
    for _ in 0..2 {
        domain.enable(&mut context, route.id()).unwrap();
        assert_eq!(
            domain.source(route.id()).unwrap(),
            aplic::Source::Enabled(route.mode())
        );
        assert_eq!(
            domain.enable(&mut context, route.id()),
            Err(aplic::Failure::Busy)
        );
        // Enable the actual device's transmitter-empty interrupt; no APLIC
        // software pending injection and no SBI console calls in this window.
        uart_write(&mut serial, 1, 2);
        uart_write(&mut serial, 0, b' ');
        let limit = counter
            .read()
            .unwrap()
            .checked_add(u64::from(counter.hertz()) * 2)
            .unwrap();
        assert_eq!(
            unsafe { receive(limit) },
            EXTERNAL,
            "supervisor external trap"
        );
        let receipt = {
            let mut receiver = domain.receiver(&mut context).unwrap();
            let claim =
                claim_consumer::receive(&mut receiver, route.id()).expect("real UART claim");
            assert_eq!(claim.priority(), 1);
            let receipt = claim.identity();
            assert!(receipt.round() > previous);
            previous = receipt.round();
            receipt
        };
        assert_eq!(
            domain.source(route.id()).unwrap(),
            aplic::Source::Masked(route.mode())
        );
        let mut receiver = domain.receiver(&mut context).unwrap();
        let mut claim = claim_consumer::pending(&mut receiver, receipt);
        assert_eq!(uart_read(&mut serial, 2) & 0x0f, 2, "UART THRE cause");
        uart_write(&mut serial, 1, 0);
        // The test has consumed this notification and disabled the actual
        // source. There is no separate software binding in this scenario.
        unsafe { consumer::finish(&mut claim, route.id(), Completion::Settled) };
    }
    assert!(domain.receiver(&mut context).unwrap().current().is_none());
    assert!(
        domain
            .receiver(&mut context)
            .unwrap()
            .claim()
            .unwrap()
            .is_none()
    );
    unsafe { domain.shut(&mut context) }.unwrap();
    assert_eq!(domain.state(), aplic::State::Shut);
    assert_eq!(domain.source(route.id()).unwrap(), aplic::Source::Vacant);
    assert_eq!(
        unsafe { domain.open(&mut context) },
        Err(aplic::Failure::Busy)
    );
    assert_eq!(
        unsafe { domain.configure(&mut context, route) },
        Err(aplic::Failure::Busy)
    );
    assert_eq!(
        domain.enable(&mut context, route.id()),
        Err(aplic::Failure::Busy)
    );
    assert!(matches!(
        domain.receiver(&mut context).unwrap().claim(),
        Err(aplic::Failure::Busy)
    ));
    unsafe { domain.shut(&mut context) }.unwrap();
    assert_eq!(domain.state(), aplic::State::Shut);
    assert_eq!(uart_read(&mut serial, 1), 0);
    // Actual controller and device effects have settled before using the firmware console.
    drop(serial);
    boot_diagnostic::write("\nPASS device-window widths=2 rejected=5\n");
    boot_diagnostic::write(
        "\nPASS aplic-direct received=2 cleared=2 stale=1 shutdown=1 states=1 rejected=1\n",
    );
}

pub fn check(mut boot: Boot<'_>) {
    assert_eq!(boot.processor(), 0);
    let mut reception = boot.take_reception().expect("major reception handoff");
    assert!(boot.take_reception().is_none());
    timer_test(&boot, &mut reception.timer, &mut reception.external);
    external_test(&boot, &mut reception.external, &mut reception.timer);
    let enabled: usize;
    unsafe {
        core::arch::asm!("csrr {}, sie", out(reg) enabled, options(nostack, nomem));
    }
    assert_eq!(enabled & ((1 << 5) | (1 << 9)), 0);
}
