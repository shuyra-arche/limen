#![no_std]
#![no_main]

use core::{arch::naked_asm, fmt::Write, ptr};
use kernel::{
    entry::boot::{self, Boot},
    metal::{
        chrono::riscv::RiscvTimebase,
        interrupt::{
            external::aplic::{self, Direct, Mode},
            internal::timer::aclint::{self, State, Timer},
        },
        platform::firmware::sbi::platform_services,
        processor::hart::riscv::description::RiscvCpus,
    },
};

const INTERRUPT: usize = 1 << 63;
const TIMER: usize = INTERRUPT | 5;
const EXTERNAL: usize = INTERRUPT | 9;

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    if let Some(mut console) = boot::console() {
        let _ = writeln!(console, "FAIL rv64-interrupt {info}");
    }
    boot::shutdown(false)
}

/// Test-owned reception window. The production bootstrap vector remains
/// terminal. This leaf uses only caller-clobbered registers; the trap lands
/// exclusively inside this window, then restores the original vector with
/// global interrupts masked. The actual IRQ backend runs after restoration.
#[unsafe(naked)]
unsafe extern "C" fn receive(until: u64) -> usize {
    naked_asm!(
        ".option push",
        ".option norvc",
        "csrr t3, stvec",
        "lla t0, 3f",
        "csrw stvec, t0",
        "csrsi sstatus, 2",
        "4: rdtime t0",
        "bltu t0, a0, 4b",
        "csrci sstatus, 2",
        "li a0, 0",
        "j 2f",
        ".balign 4",
        "3: csrr a0, scause",
        "lla t0, 2f",
        "csrw sepc, t0",
        "li t0, 32",
        "csrc sstatus, t0",
        "sret",
        "2: csrci sstatus, 2",
        "csrw stvec, t3",
        "ret",
        ".option pop",
    )
}

fn timer_test(boot: &Boot<'_>) {
    let tree = boot.device_tree_description().expect("trusted RV64 DTB");
    let cpus = RiscvCpus::new(tree).unwrap().unwrap();
    let timebase = RiscvTimebase::describe(&cpus).unwrap().unwrap();
    let description = aclint::Description::read(&tree, boot.hart() as u64)
        .unwrap()
        .expect("ACLINT machine timer");
    assert_eq!(description.clock_scope(), timebase.source());
    assert_eq!(description.counter().length().get(), 8);
    assert_eq!(description.comparator().length().get(), 8);
    assert_eq!(description.hart(), boot.hart() as u64);
    // The runner's exact machine configuration has ACLINT and disables Sstc.
    // OpenSBI owns the machine comparator; the guest never maps or writes it.
    let services = platform_services();
    let mut timer = unsafe { Timer::admit(description, timebase, services) }.unwrap();
    timer.shut().unwrap();
    assert_eq!(timer.state(), State::Idle);
    assert!(!timer.pending().unwrap());
    let hertz = u64::from(timebase.hertz());
    for round in 0..2 {
        let now = timer.counter().unwrap();
        let target = now.checked_add(hertz / 20).unwrap();
        let limit = now.checked_add(hertz).unwrap();
        if round == 0 {
            let later = limit.checked_add(hertz).unwrap();
            timer.arm(later).unwrap();
            assert!(matches!(timer.arm(target), Err(aclint::Failure::Busy)));
        }
        // Replace the old comparison directly. In round two this also clears
        // the pending interrupt from round one without an intervening shut.
        timer.rearm(target).unwrap();
        assert_eq!(timer.state(), State::Armed(target));
        assert!(!timer.pending().unwrap());
        assert_eq!(unsafe { receive(limit) }, TIMER, "supervisor timer trap");
        assert!(timer.pending().unwrap());
        assert!(timer.due().unwrap());
    }
    timer.shut().unwrap();
    assert_eq!(timer.state(), State::Idle);
    assert!(!timer.pending().unwrap());
    boot::write("PASS aclint-timer received=2 cleared=2\n");
}

fn uart_read(base: usize, register: usize) -> u8 {
    // The test owns the QEMU ns16550a byte register window during this step.
    unsafe { ptr::without_provenance::<u8>(base + register).read_volatile() }
}

fn uart_write(base: usize, register: usize, value: u8) {
    unsafe {
        ptr::without_provenance_mut::<u8>(base + register).write_volatile(value);
        core::arch::asm!("fence iorw, iorw", options(nostack));
    }
}

fn external_test(boot: &Boot<'_>) {
    let tree = boot.device_tree_description().unwrap();
    let cpus = RiscvCpus::new(tree).unwrap().unwrap();
    let timebase = RiscvTimebase::describe(&cpus).unwrap().unwrap();
    let description = aplic::Description::read(&tree, boot.hart() as u64)
        .unwrap()
        .expect("supervisor direct APLIC");
    assert_eq!(description.contexts(), 1);
    let uart = tree
        .find_path(b"/soc/serial@10000000")
        .unwrap()
        .expect("QEMU UART");
    assert!(uart.compatible(b"ns16550a").unwrap());
    let range = uart.register(0).unwrap();
    assert!(range.length().get() >= 8);
    let serial = usize::try_from(range.start().get()).unwrap();
    let route = description.route(&tree, uart).unwrap();
    assert_eq!(route.mode(), Mode::LevelHigh);
    assert_eq!(uart_read(serial, 3) & 0x80, 0, "DLAB must be clear");
    assert_eq!(uart_read(serial, 1), 0, "firmware console must be polling");
    let base = usize::try_from(description.registers().start().get()).unwrap();
    // The single-Hart QEMU board, initial bare mapping, quiet device and
    // supervisor domain delegated by OpenSBI establish this test's ownership.
    let mut direct = unsafe { Direct::admit(description, base) }.unwrap();
    unsafe { direct.configure(route) }.unwrap();
    unsafe { direct.open() }.unwrap();
    assert!(direct.claim().unwrap().is_none());
    let mut previous = 0;
    for _ in 0..2 {
        direct.enable(route.id()).unwrap();
        // Enable the actual device's transmitter-empty interrupt; no APLIC
        // software pending injection and no SBI console calls in this window.
        uart_write(serial, 1, 2);
        uart_write(serial, 0, b' ');
        let limit = unsafe { timebase.counter() }
            .checked_add(u64::from(timebase.hertz()) * 2)
            .unwrap();
        assert_eq!(unsafe { receive(limit) }, EXTERNAL, "supervisor external trap");
        {
            let claim = direct.claim().unwrap().expect("real UART claim");
            assert_eq!(claim.source(), route.id());
            assert_eq!(claim.priority(), 1);
            assert!(claim.round() > previous);
            previous = claim.round();
            // Borrow ends here, but the persistent hardware obligation remains.
        }
        assert!(matches!(direct.claim(), Err(aplic::Failure::Busy)));
        let mut claim = direct.current().expect("retained original claim");
        assert_eq!(claim.round(), previous);
        assert_eq!(uart_read(serial, 2) & 0x0f, 2, "UART THRE cause");
        uart_write(serial, 1, 0);
        // The test has consumed this notification and disabled the actual
        // source. There is no separate software binding in this scenario.
        unsafe { claim.settle() }.unwrap();
        assert_eq!(unsafe { claim.settle() }, Err(aplic::Failure::Busy));
    }
    assert!(direct.current().is_none());
    assert!(direct.claim().unwrap().is_none());
    unsafe { direct.shut() }.unwrap();
    assert_eq!(uart_read(serial, 1), 0);
    boot::write("\nPASS aplic-direct received=2 cleared=2 stale=1 shutdown=1\n");
}

#[unsafe(no_mangle)]
extern "Rust" fn limen_platform_ready(boot: Boot<'_>) -> ! {
    assert_eq!(boot.hart(), 0);
    timer_test(&boot);
    external_test(&boot);
    let enabled: usize;
    unsafe {
        core::arch::asm!("csrr {}, sie", out(reg) enabled, options(nostack, nomem));
    }
    assert_eq!(enabled & ((1 << 5) | (1 << 9)), 0);
    boot::shutdown(true)
}
