//! QEMU virt hands the guest an identity-mapped physical GIC with a single
//! security state. Keep the PE IRQ/FIQ masks closed and poll its real IAR.
use kernel::metal::interrupt::{AbsoluteTimer, Completion};

#[path = "../../../../support/claim.rs"]
mod claim_consumer;
#[path = "../../../../support/interrupt.rs"]
mod consumer;
#[path = "../../../../support/route.rs"]
mod route_consumer;
#[path = "../../../../support/timer.rs"]
mod timer_consumer;

use core::{
    arch::asm,
    fmt::Write,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
use kernel::{
    entry::boot,
    metal::{
        chrono::aarch64::GenericCounter,
        interrupt::aarch64::{
            gic::{
                GicV3,
                cpu_interface::{CpuInterface, available},
                distributor::{Barrier, Distributor, Route, Slot, State as SpiState},
                reception::State,
                redistributor::{
                    Frame, FrameHandoff, PrivateInterrupts, Redistributor, Slot as FrameSlot,
                },
                sgi::Sgi,
            },
            timer::{Failure as TimerFailure, GenericTimer, VirtualTimer},
        },
        memory::mmu::access::device::DeviceWindow,
        platform::{description::devicetree::DeviceTree, firmware::psci::PsciDescription},
        processor::hart::aarch64::description::Aarch64Cpus,
        sync::LeafLock,
    },
};
use limen_treaty::model::{InternalAddress, PhysicalRange};

fn window(range: PhysicalRange) -> DeviceWindow<'static> {
    unsafe { DeviceWindow::admit(range, InternalAddress::new(range.start().get())).unwrap() }
}

fn sync() {
    unsafe { asm!("dsb sy", "isb", options(nostack)) }
}

const UART_DATA: usize = 0;
const UART_CONTROL: usize = 0x30;
const UART_MASK: usize = 0x38;
const UART_STATUS: usize = 0x40;
const UART_CLEAR: usize = 0x44;
const UART_ENABLE_TRANSMIT: u32 = 1 | 1 << 8;
const TX_INTERRUPT: u32 = 1 << 5;

fn clear_transmit(uart: &mut DeviceWindow<'_>) {
    unsafe {
        uart.write_u32(UART_MASK, 0).unwrap();
        uart.write_u32(UART_CLEAR, TX_INTERRUPT).unwrap();
    }
    sync();
    assert_eq!(unsafe { uart.read_u32(UART_STATUS) }.unwrap(), 0);
}

fn transmit(uart: &mut DeviceWindow<'_>) {
    unsafe {
        uart.write_u32(UART_MASK, TX_INTERRUPT).unwrap();
        uart.write_u32(UART_DATA, u32::from(b'\n')).unwrap();
    }
    sync();
}

fn wait_for_claim(receiver: &mut CpuInterface<'_>, expected: usize) {
    let start: u64;
    let frequency: u64;
    unsafe {
        asm!("mrs {}, CNTVCT_EL0", "mrs {}, CNTFRQ_EL0", out(reg) start, out(reg) frequency, options(nostack))
    };
    loop {
        if claim_consumer::receive(receiver, expected as u32).is_some() {
            return;
        }
        let now: u64;
        unsafe { asm!("mrs {}, CNTVCT_EL0", out(reg) now, options(nostack)) };
        assert!(
            now.wrapping_sub(start) < frequency * 3,
            "GIC reception timeout"
        );
    }
}

static MINOR: boot::Minor = boot::Minor::new();
static MINOR_FRAME: LeafLock<Option<FrameHandoff<'static>>> = LeafLock::new(None);
static MINOR_OBSERVED: AtomicBool = AtomicBool::new(false);
static MINOR_SIGNAL: LeafLock<Option<Sgi<'static>>> = LeafLock::new(None);
static SIGNAL_ISSUED: AtomicBool = AtomicBool::new(false);
static SIGNAL_OBSERVED: AtomicBool = AtomicBool::new(false);
const SIGNAL_ID: u8 = 12;

fn wake(frame: &mut Frame<'_>) {
    for _ in 0..4096 {
        if frame.wake().is_ok() {
            break;
        }
    }
    frame.wake().unwrap();
    assert!(frame.awake());
}

fn observe_minor(hart: usize, _: usize) -> ! {
    let handoff = MINOR_FRAME
        .acquire(Option::take)
        .unwrap()
        .expect("original frame handoff");
    // The trusted virt image retains the identity map on both PEs. The
    // major relinquished this exact disjoint physical range before PSCI.
    let range = handoff.physical();
    let mut frame = handoff
        .attach(window(range))
        .unwrap_or_else(|(_, _, reason)| panic!("minor frame attachment: {reason:?}"));
    assert_eq!(frame.affinity(), hart);
    assert_eq!(frame.physical(), range);
    assert!(unsafe { available() });
    wake(&mut frame);
    // Admit the minor's real EL1 system-register interface and its Group 1
    // priority mask. Ordinary IRQ/FIQ remain masked while the guest polls.
    unsafe {
        let sre: u64;
        asm!("mrs {}, ICC_SRE_EL1", out(reg) sre, options(nostack, nomem));
        asm!(
            "msr ICC_SRE_EL1, {sre}", "isb",
            "msr ICC_PMR_EL1, {pmr}", "msr ICC_IGRPEN1_EL1, {group}", "isb",
            sre = in(reg) sre | 1,
            pmr = in(reg) 0x80u64,
            group = in(reg) 1u64,
            options(nostack)
        );
    }
    let mut receiver = unsafe { CpuInterface::admit(frame) }
        .unwrap_or_else(|(_, reason)| panic!("original SGI CPU Interface: {reason:?}"));
    let signal = unsafe { receiver.reserve_sgi(SIGNAL_ID, 0x10) }.expect("original minor SGI");
    MINOR_SIGNAL
        .acquire(|slot| {
            assert!(slot.is_none());
            *slot = Some(signal);
        })
        .unwrap();
    MINOR_OBSERVED.store(true, Ordering::Release);
    while !SIGNAL_ISSUED.load(Ordering::Acquire) {
        unsafe { asm!("wfe", options(nostack, nomem)) };
    }
    let mut signal = MINOR_SIGNAL
        .acquire(Option::take)
        .unwrap()
        .expect("original sent SGI");
    wait_for_claim(&mut receiver, usize::from(SIGNAL_ID));
    let mut claim = receiver.claim().expect("original minor SGI");
    assert_eq!(claim.intid(), u32::from(signal.intid()));
    claim.complete_sgi(&mut signal).unwrap();
    assert!(claim.complete_sgi(&mut signal).is_err());
    let mut frame = receiver
        .release()
        .unwrap_or_else(|_| panic!("minor SGI remains live"));
    frame.retire_sgi(signal).unwrap();
    SIGNAL_OBSERVED.store(true, Ordering::Release);
    loop {
        // Keep the original Frame reserved for this PE until guest shutdown.
        core::hint::black_box(&mut frame);
        unsafe { asm!("wfe", options(nostack, nomem)) };
    }
}

fn start_minor(tree: &DeviceTree<'_>, hart: usize, frame: Frame<'static>) {
    let psci = PsciDescription::read(tree)
        .unwrap()
        .expect("GIC fixture PSCI");
    let psci = unsafe { psci.verify() }.unwrap();
    unsafe { psci.verify_affinity(hart, false) }.unwrap();
    MINOR_FRAME
        .acquire(|slot| {
            assert!(slot.is_none());
            *slot = Some(frame.handoff());
        })
        .unwrap();
    let mut startup = boot::Startup::Psci(psci);
    unsafe { MINOR.start(hart, observe_minor, 0, &mut startup) }.unwrap();
    let start: u64;
    let frequency: u64;
    unsafe {
        asm!("mrs {}, CNTVCT_EL0", "mrs {}, CNTFRQ_EL0", out(reg) start, out(reg) frequency, options(nostack))
    };
    while !MINOR_OBSERVED.load(Ordering::Acquire) {
        let now: u64;
        unsafe { asm!("mrs {}, CNTVCT_EL0", out(reg) now, options(nostack)) };
        assert!(
            now.wrapping_sub(start) < frequency * 3,
            "minor GICR observation timeout"
        );
    }
    assert!(MINOR.ready());
    unsafe { psci.verify_affinity(hart, true) }.unwrap();
}

fn check_signal(frame: Frame<'static>) -> Frame<'static> {
    let mut sender = unsafe { CpuInterface::admit(frame) }
        .unwrap_or_else(|(_, reason)| panic!("original SGI sender: {reason:?}"));
    let mut signal = MINOR_SIGNAL
        .acquire(Option::take)
        .unwrap()
        .expect("admitted minor SGI");
    sender.send_sgi(&mut signal).unwrap();
    assert!(sender.send_sgi(&mut signal).is_err());
    MINOR_SIGNAL
        .acquire(|slot| {
            assert!(slot.is_none());
            *slot = Some(signal);
        })
        .unwrap();
    SIGNAL_ISSUED.store(true, Ordering::Release);
    unsafe { asm!("sev", options(nostack, nomem)) };
    let start: u64;
    let frequency: u64;
    unsafe {
        asm!("mrs {}, CNTVCT_EL0", "mrs {}, CNTFRQ_EL0", out(reg) start, out(reg) frequency, options(nostack))
    };
    while !SIGNAL_OBSERVED.load(Ordering::Acquire) {
        let now: u64;
        unsafe { asm!("mrs {}, CNTVCT_EL0", out(reg) now, options(nostack)) };
        assert!(
            now.wrapping_sub(start) < frequency * 3,
            "minor SGI completion timeout"
        );
    }
    boot::write("PASS arm-sgi target=1 claim=1 eoi=1 retired=1\n");
    sender
        .release()
        .unwrap_or_else(|_| panic!("major SGI interface busy"))
}

fn check_virtual_timer(
    tree: &DeviceTree<'_>,
    domain: GicV3<'_>,
    major: usize,
    mut frame: Frame<'static>,
) -> Frame<'static> {
    let counter = GenericCounter::read(tree).unwrap().expect("virt counter");
    let bindings = PrivateInterrupts::read(tree, counter.source(), domain).unwrap();
    let timer = GenericTimer::describe(tree, &bindings).unwrap();
    let role = timer.virtual_interrupt();
    let mut ppi = unsafe { frame.reserve_ppi(&bindings, role, 0x10) }.unwrap();
    let mut receiver = unsafe { CpuInterface::admit(frame) }
        .unwrap_or_else(|(_, reason)| panic!("timer PPI CPU Interface: {reason:?}"));
    let clock = unsafe { counter.clock(major) }.unwrap();
    for delay in [Duration::from_secs(5), Duration::from_millis(10)] {
        receiver.prepare_ppi(&mut ppi).unwrap();
        let mut timer = unsafe { VirtualTimer::admit(timer, &clock, &mut ppi) }.unwrap();
        timer_consumer::arm(&clock, &mut timer, delay);
        if delay == Duration::from_secs(5) {
            assert_eq!(timer.stop(), Err(TimerFailure::Busy));
            assert_eq!(
                timer.rearm_at(
                    clock
                        .sample()
                        .unwrap()
                        .after(Duration::from_secs(8))
                        .unwrap()
                ),
                Err(TimerFailure::Busy)
            );
            timer
                .rearm_at(
                    clock
                        .sample()
                        .unwrap()
                        .after(Duration::from_millis(1))
                        .unwrap(),
                )
                .unwrap();
        }
        assert!(timer.stopped_ppi().is_none());
        wait_for_claim(&mut receiver, role.intid as usize);
        timer_consumer::confirm_due(&timer);
        assert_eq!(timer.pending(), Ok(true));
        timer.stop().unwrap();
        assert_eq!(
            AbsoluteTimer::arm_at(
                &mut timer,
                clock
                    .sample()
                    .unwrap()
                    .after(Duration::from_millis(10))
                    .unwrap(),
            ),
            Err(TimerFailure::Busy)
        );
        let mut claim = receiver.claim().expect("original virtual timer claim");
        assert_eq!(claim.intid(), role.intid);
        claim
            .complete_ppi(timer.stopped_ppi().expect("original stopped timer input"))
            .unwrap();
        assert!(timer.stopped_ppi().is_none());
        assert_eq!(receiver.state(), State::Empty);
    }
    let mut frame = receiver
        .release()
        .unwrap_or_else(|_| panic!("timer receiver remains live"));
    for _ in 0..4096 {
        match unsafe { frame.retire_ppi(ppi) } {
            Ok(()) => {
                boot::write("PASS arm-virtual-timer programmed=1 ppi=1 eoi=1 retired=1 common=1\n");
                return frame;
            }
            Err((returned, _)) => ppi = returned,
        }
    }
    panic!("virtual timer PPI could not retire");
}

pub(super) fn check(tree: &DeviceTree<'_>, controller: GicV3<'_>, major: usize) {
    let device = tree
        .find_path(b"/pl011@9000000")
        .unwrap()
        .expect("virt console UART");
    let second = tree
        .find_path(b"/pl011@9040000")
        .unwrap()
        .expect("virt secondary UART");
    for uart in [device, second] {
        assert!(uart.compatible(b"arm,pl011").unwrap());
        assert!(uart.enabled().unwrap());
    }
    let route = Route::read(tree, device.id(), controller, 0).unwrap();
    let second_route = Route::read(tree, second.id(), controller, 0).unwrap();
    let id = route.intid();
    let second_id = second_route.intid();
    assert_ne!(second_id, id);
    assert_eq!(second_id / 32, id / 32, "sources share a group word");
    assert_eq!(
        second_id / 16,
        id / 16,
        "sources share a configuration word"
    );
    let mut console = window(device.register(0).unwrap());
    let mut secondary = window(second.register(0).unwrap());
    let secondary_control = unsafe { secondary.read_u32(UART_CONTROL).unwrap() };
    unsafe {
        secondary
            .write_u32(UART_CONTROL, secondary_control | UART_ENABLE_TRANSMIT)
            .unwrap()
    };
    clear_transmit(&mut console);
    clear_transmit(&mut secondary);
    let mut distributor = window(controller.distributor());
    let cpus = Aarch64Cpus::read(tree).unwrap().expect("GIC fixture CPUs");
    let minor = cpus
        .candidates()
        .find(|cpu| cpu.enabled && cpu.starts_with_psci && cpu.affinity != major)
        .expect("second real PE")
        .affinity;
    let mut frame_slots = [const { FrameSlot::EMPTY }; 8];
    let frames = unsafe {
        Redistributor::admit(
            controller,
            window(controller.redistributor()),
            &mut frame_slots,
        )
    }
    .expect("complete GICR topology");
    let (mut local_frame, mut minor_frame) = (None, None);
    for frame in frames {
        if frame.affinity() == major {
            assert!(local_frame.replace(frame).is_none());
        } else if frame.affinity() == minor {
            assert!(minor_frame.replace(frame).is_none());
        }
    }
    let mut local_frame = local_frame.expect("major GICR");
    let minor_frame = minor_frame.expect("minor GICR");
    assert_eq!(local_frame.domain(), minor_frame.domain());
    assert!(!local_frame.physical().overlaps(minor_frame.physical()));
    start_minor(tree, minor, minor_frame);
    wake(&mut local_frame);
    // ExitBootServices has relinquished the physical GIC. The major's CPU
    // masks remain closed. Restore forwarding and PMR after UART completion.
    let old_control = unsafe { distributor.read_u32(0).unwrap() };
    assert_eq!(old_control & (1 << 6), 1 << 6);
    let old_mode: u64;
    unsafe { asm!("mrs {}, ICC_CTLR_EL1", out(reg) old_mode, options(nostack)) };
    let old_group: u64;
    let old_priority: u64;
    unsafe {
        asm!("mrs {}, ICC_IGRPEN1_EL1", "mrs {}, ICC_PMR_EL1", out(reg) old_group, out(reg) old_priority, options(nostack));
    }
    sync();
    for split in [false, true] {
        let requested = (old_mode & !2) | (u64::from(split) << 1);
        unsafe { asm!("msr ICC_CTLR_EL1, {}", "isb", in(reg) requested, options(nostack)) };
        unsafe {
            distributor.write_u32(0, old_control | (1 << 1)).unwrap();
            asm!("msr ICC_PMR_EL1, {}", "msr ICC_IGRPEN1_EL1, {}", "isb", in(reg) 0x80u64, in(reg) 1u64, options(nostack));
        }
        sync();
        let mut receiver = unsafe { CpuInterface::admit(local_frame) }
            .unwrap_or_else(|(_, reason)| panic!("local GIC receiver: {reason:?}"));
        assert!(receiver.receive().unwrap().is_none());
        let mut slots = [const { Slot::EMPTY }; 2];
        let mut spi = unsafe {
            Distributor::admit(
                controller,
                distributor.reborrow(),
                Barrier::native(),
                &mut slots,
            )
        }
        .expect("physical Distributor");
        unsafe {
            spi.reserve(route).unwrap();
            spi.reserve(second_route).unwrap();
            assert!(spi.reserve(second_route).is_err());
            route_consumer::publish(&mut spi.configure(id, major as u64, 0x10).unwrap());
            assert_eq!(spi.state(second_id), Ok(SpiState::Masked));
            route_consumer::publish(&mut spi.configure(second_id, major as u64, 0x20).unwrap());
        }
        for (input, uart) in [(id, &mut console), (second_id, &mut secondary)] {
            assert_eq!(spi.state(input), Ok(SpiState::Enabled));
            transmit(uart);
            wait_for_claim(&mut receiver, input);
            assert_eq!(receiver.state(), State::Claimed(input as u32));
            let mut claim = claim_consumer::pending(&mut receiver, input as u32);
            assert_eq!(claim.intid(), input as u32);
            assert!(unsafe { claim.deactivate() }.is_err());
            assert_eq!(unsafe { uart.read_u32(UART_STATUS) }.unwrap(), TX_INTERRUPT);
            clear_transmit(uart);
            for _ in 0..4096 {
                if spi.disable(input).is_ok() {
                    break;
                }
            }
            assert_eq!(spi.state(input), Ok(SpiState::Masked));
            if !split {
                unsafe { consumer::finish(&mut claim, input as u32, Completion::Settled) };
            } else {
                unsafe { claim.drop_priority().unwrap() };
                assert!(unsafe { claim.drop_priority() }.is_err());
                assert_eq!(spi.active(input), Ok(true));
                unsafe { claim.deactivate().unwrap() };
                assert!(unsafe { claim.deactivate() }.is_err());
            }
            assert_eq!(spi.active(input), Ok(false));
            assert_eq!(receiver.state(), State::Empty);
            unsafe { spi.retire(input).unwrap() };
        }
        local_frame = receiver
            .release()
            .unwrap_or_else(|_| panic!("original GIC reception remains live"));
    }
    local_frame = check_virtual_timer(tree, controller, major, local_frame);
    local_frame = check_signal(local_frame);
    assert!(local_frame.awake());
    boot::write("PASS gicr frames=2 independent=1\n");
    boot::write("PASS external-route published=2 repeated=0 independent=1\n");
    let bank = id / 32 * 4;
    let bit = (1 << (id % 32)) | (1 << (second_id % 32));
    assert_eq!(
        unsafe { distributor.read_u32(0x300 + bank) }.unwrap() & bit,
        0
    );
    assert_eq!(
        unsafe { distributor.read_u32(0x100 + bank) }.unwrap() & bit,
        0
    );
    unsafe {
        secondary
            .write_u32(UART_CONTROL, secondary_control)
            .unwrap();
        asm!("msr ICC_CTLR_EL1, {}", "isb", in(reg) old_mode, options(nostack));
        distributor.write_u32(0, old_control).unwrap();
        asm!("msr ICC_PMR_EL1, {}", "msr ICC_IGRPEN1_EL1, {}", "isb", in(reg) old_priority, in(reg) old_group, options(nostack));
    }
    sync();
    boot::console()
        .unwrap()
        .write_str("PASS virt interrupt uart-primary=2 uart-secondary=2 shared-words=1 iar=4 eoi=4 dir=2 masked=1\n")
        .unwrap();
}
