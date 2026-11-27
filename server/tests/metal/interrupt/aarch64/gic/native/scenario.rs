//! QEMU virt hands the guest an identity-mapped physical GIC with a single
//! security state. Keep the PE IRQ/FIQ masks closed and poll its real IAR.

use kernel::entry::hook::Binding;
use kernel::metal::interrupt::treaty::{AbsoluteTimer, ClaimId, Completion, HardwareClaim};
use kernel::metal::platform::boot::treaty as boot_diagnostic;
use kernel::metal::processor::treaty::{
    InstallError, Irq, LocalStorage, Minor, RuntimeTrap, Stack, Startup,
};

#[path = "spi.rs"]
mod spi;

#[path = "../../../claim.rs"]
mod claim_consumer;
#[path = "../../../completion.rs"]
mod consumer;
#[path = "../../../route.rs"]
mod route_consumer;
#[path = "../../../timer.rs"]
mod timer_consumer;

use core::{
    arch::asm,
    cell::UnsafeCell,
    fmt::Write,
    ptr,
    sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering},
    time::Duration,
};
use kernel::metal::{
    chrono::aarch64::description::GenericCounter,
    interrupt::aarch64::{
        gic::{
            GicV3,
            cpu_interface::{CpuInterface, Wake, available},
            distributor::{Barrier, Distributor, Route, Slot, State as SpiState},
            reception::{Failure as ReceptionFailure, State},
            redistributor::{
                Frame, FrameHandoff, Redistributor, Slot as FrameSlot,
                input::description::PrivateInterrupts,
            },
            sgi::Sgi,
        },
        timer::{Failure as TimerFailure, GenericTimer, VirtualTimer},
    },
    memory::access::treaty::device::DeviceWindow,
    platform::{
        description::treaty::devicetree::DeviceTree, firmware::treaty::psci::PsciDescription,
    },
    processor::description::aarch64::cpu::Aarch64Cpus,
    sync::treaty::{LeafLock, Supply, TurnAcquireError, TurnLock},
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

fn wait_for_claim(receiver: &mut CpuInterface<'_>, expected: usize) -> ClaimId {
    let start: u64;
    let frequency: u64;
    unsafe {
        asm!("mrs {}, CNTVCT_EL0", "mrs {}, CNTFRQ_EL0", out(reg) start, out(reg) frequency, options(nostack))
    };
    loop {
        if let Some(claim) = claim_consumer::receive(receiver, expected as u32) {
            return claim.identity();
        }
        let now: u64;
        unsafe { asm!("mrs {}, CNTVCT_EL0", out(reg) now, options(nostack)) };
        assert!(
            now.wrapping_sub(start) < frequency * 3,
            "GIC reception timeout"
        );
    }
}

static MINOR_STACK: Stack<{ 32 * 1024 }> = Stack::new();
static MINOR_LOCAL: LocalStorage<5> = LocalStorage::new(Supply::new());
static MINOR: Minor = Minor::new(&MINOR_STACK, &MINOR_LOCAL);
static MINOR_FRAME: LeafLock<Option<FrameHandoff<'static>>> = LeafLock::new(None);
static MINOR_SIGNAL: LeafLock<Option<Sgi<'static>>> = LeafLock::new(None);
static SIGNAL_READY: AtomicUsize = AtomicUsize::new(0);
static FINAL_CHECK: AtomicUsize = AtomicUsize::new(0);
static SIGNAL_WORK: AtomicUsize = AtomicUsize::new(0);
static SIGNAL_ISSUED: AtomicUsize = AtomicUsize::new(0);
static SIGNAL_OBSERVED: AtomicUsize = AtomicUsize::new(0);
const SIGNAL_ID: u8 = 12;

struct Reception {
    processor: usize,
    receiver: CpuInterface<'static>,
    signal: Sgi<'static>,
}

struct LocalReception(UnsafeCell<Option<Reception>>);

// Only the minor accesses the slot. Ordinary access is while IRQ is masked,
// and the handler's borrow ends before the original continuation resumes.
unsafe impl Sync for LocalReception {}

static MINOR_IRQ: Binding<LocalReception> =
    Binding::new(LocalReception(UnsafeCell::new(None)), handle_irq);
static MINOR_VECTORS: RuntimeTrap = RuntimeTrap::new(MINOR_IRQ.entry());
static RETURN_READY: AtomicBool = AtomicBool::new(false);
static IRQ_TURN: TurnLock<()> = TurnLock::new(());

fn handle_irq(owner: &LocalReception, irq: Irq<'_>) {
    assert_eq!(IRQ_TURN.acquire(|_| ()), Err(TurnAcquireError::Context));
    let original = unsafe { (&mut *owner.0.get()).as_mut().expect("prepared SGI") };
    assert_eq!(irq.processor().get(), original.processor);
    let mut claim = original
        .receiver
        .receive()
        .unwrap()
        .expect("actual SGI claim");
    assert_eq!(claim.intid(), u32::from(original.signal.intid()));
    claim.complete_sgi(&mut original.signal).unwrap();
    assert!(claim.complete_sgi(&mut original.signal).is_err());
    // The interrupted register values are live across this ordinary call ABI.
    unsafe {
        asm!("movi v0.16b, #0x11", "mov x8, #0x42", out("v0") _, out("x8") _, options(nostack, nomem));
    }
    RETURN_READY.store(true, Ordering::Release);
}

fn check_return() {
    let state: usize;
    let integers: usize;
    let vectors: usize;
    let stack: usize;
    unsafe {
        asm!(
            "mov x8, #0xabcd",
            "mov x9, sp",
            // Make the interrupted SP unaligned without using it for memory.
            "sub sp, sp, #8",
            "movi v0.16b, #0x55",
            "msr daifclr, #2",
            "isb",
            "2: ldarb w10, [{ready}]",
            "cbz w10, 2b",
            "mrs {state}, daif",
            "msr daifset, #2",
            "add sp, sp, #8",
            "mov x10, sp",
            "cmp x9, x10",
            "cset {stack}, eq",
            "mov x10, #0xabcd",
            "cmp x8, x10",
            "cset {integers}, eq",
            "movi v1.16b, #0x55",
            "cmeq v1.16b, v0.16b, v1.16b",
            "uminv b1, v1.16b",
            "umov w10, v1.b[0]",
            "cmp w10, #0xff",
            "cset {vectors}, eq",
            ready = in(reg) RETURN_READY.as_ptr(),
            state = lateout(reg) state,
            integers = lateout(reg) integers,
            vectors = lateout(reg) vectors,
            stack = lateout(reg) stack,
            out("x8") _,
            out("x9") _,
            out("x10") _,
            out("v0") _,
            out("v1") _,
            options(nostack),
        );
    }
    assert_eq!(state & 0x80, 0, "the interrupted IRQ mask was restored");
    assert_eq!((integers, vectors, stack), (1, 1, 1));
}

fn wake(frame: &mut Frame<'_>) {
    for _ in 0..4096 {
        if frame.wake().is_ok() {
            break;
        }
    }
    frame.wake().unwrap();
    assert!(frame.awake());
}

fn observe_minor(processor: usize, _: usize) -> ! {
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
    assert_eq!(frame.affinity(), processor);
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
    assert_eq!(unsafe { MINOR_VECTORS.install() }, Ok(()));
    assert_eq!(
        unsafe { MINOR_VECTORS.install() },
        Err(InstallError::Occupied)
    );
    for round in 1..=3 {
        let mut receiver = unsafe { CpuInterface::admit(frame) }
            .unwrap_or_else(|(_, reason)| panic!("original SGI CPU Interface: {reason:?}"));
        let signal = unsafe { receiver.reserve_sgi(SIGNAL_ID, 0x10) }.expect("original minor SGI");
        MINOR_SIGNAL
            .acquire(|slot| {
                assert!(slot.is_none());
                *slot = Some(signal);
            })
            .unwrap();
        RETURN_READY.store(false, Ordering::Relaxed);
        assert_eq!(
            unsafe { receiver.wait_for_sgi(SIGNAL_ID + 1, || false) },
            Err(ReceptionFailure::Scope)
        );
        assert_eq!(
            IRQ_TURN.acquire(|_| unsafe { receiver.wait_for_sgi(SIGNAL_ID, || false) }),
            Ok(Err(ReceptionFailure::Busy)),
        );
        unsafe { asm!("msr ICC_PMR_EL1, {}", "isb", in(reg) 0x10u64, options(nostack)) };
        assert_eq!(
            unsafe { receiver.wait_for_sgi(SIGNAL_ID, || false) },
            Err(ReceptionFailure::Busy)
        );
        unsafe { asm!("msr ICC_PMR_EL1, {}", "isb", in(reg) 0x80u64, options(nostack)) };
        SIGNAL_READY.store(round, Ordering::Release);
        check_idle(&mut receiver, round);
        assert!(!RETURN_READY.load(Ordering::Acquire));
        let signal = MINOR_SIGNAL
            .acquire(Option::take)
            .unwrap()
            .expect("original sent SGI");
        unsafe {
            MINOR_IRQ.owner().0.get().write(Some(Reception {
                processor,
                receiver,
                signal,
            }))
        };
        assert_eq!(IRQ_TURN.acquire(|_| ()), Ok(()));
        check_return();
        assert_eq!(IRQ_TURN.acquire(|_| ()), Ok(()));
        let Reception {
            mut receiver,
            signal,
            ..
        } = unsafe {
            (&mut *MINOR_IRQ.owner().0.get())
                .take()
                .expect("returned local SGI")
        };
        assert_eq!(receiver.state(), State::Empty);
        assert!(receiver.receive().unwrap().is_none());
        frame = receiver
            .release()
            .unwrap_or_else(|_| panic!("minor SGI remains live"));
        frame.retire_sgi(signal).unwrap();
        SIGNAL_OBSERVED.store(round, Ordering::Release);
    }
    spi::on_minor(frame)
}

fn check_idle(receiver: &mut CpuInterface<'_>, round: usize) {
    if round == 1 {
        await_round(&SIGNAL_ISSUED, round);
        assert_eq!(
            unsafe { receiver.wait_for_sgi(SIGNAL_ID, || signal_work(round)) },
            Ok(Wake::Ready),
        );
        return;
    }
    if round == 2 {
        let wake = unsafe {
            receiver.wait_for_sgi(SIGNAL_ID, || {
                assert!(!signal_work(round));
                FINAL_CHECK.store(round, Ordering::Release);
                // The real send wins after the work snapshot but before the
                // receiver's last hardware pending check.
                await_round(&SIGNAL_ISSUED, round);
                false
            })
        };
        assert_eq!(wake, Ok(Wake::Pending));
        return;
    }

    let limit = time_count() + 3 * time_frequency();
    let mut announced = false;
    loop {
        let wake = unsafe {
            receiver.wait_for_sgi(SIGNAL_ID, || {
                if !announced {
                    assert!(!signal_work(round));
                    FINAL_CHECK.store(round, Ordering::Release);
                    announced = true;
                    return false;
                }
                signal_work(round)
            })
        }
        .unwrap();
        if wake != Wake::Recheck {
            break;
        }
        assert!(time_count() < limit, "SGI idle wake timeout");
    }
    await_round(&SIGNAL_ISSUED, round);
}

fn start_minor(tree: &DeviceTree<'_>, processor: usize, frame: Frame<'static>) {
    let psci = PsciDescription::read(tree)
        .unwrap()
        .expect("GIC fixture PSCI");
    let psci = unsafe { psci.verify() }.unwrap();
    unsafe { psci.verify_affinity(processor, false) }.unwrap();
    MINOR_FRAME
        .acquire(|slot| {
            assert!(slot.is_none());
            *slot = Some(frame.handoff());
        })
        .unwrap();
    let mut startup = Startup::Psci(psci);
    unsafe { MINOR.start(processor, observe_minor, 0, &mut startup) }.unwrap();
    await_round(&SIGNAL_READY, 1);
    assert!(MINOR.ready());
    unsafe { psci.verify_affinity(processor, true) }.unwrap();
}

fn time_count() -> u64 {
    let count;
    unsafe { asm!("mrs {}, CNTVCT_EL0", out(reg) count, options(nostack, nomem)) };
    count
}

fn time_frequency() -> u64 {
    let frequency;
    unsafe { asm!("mrs {}, CNTFRQ_EL0", out(reg) frequency, options(nostack, nomem)) };
    frequency
}

fn signal_work(round: usize) -> bool {
    SIGNAL_WORK.load(Ordering::Acquire) == round
}

fn await_round(state: &AtomicUsize, round: usize) {
    let limit = time_count() + 3 * time_frequency();
    while state.load(Ordering::Acquire) != round {
        assert!(time_count() < limit, "peer SGI round timeout");
        core::hint::spin_loop();
    }
}

fn check_signal(frame: Frame<'static>) -> Frame<'static> {
    let mut sender = unsafe { CpuInterface::admit(frame) }
        .unwrap_or_else(|(_, reason)| panic!("original SGI sender: {reason:?}"));
    for round in 1..=3 {
        await_round(&SIGNAL_READY, round);
        if round > 1 {
            await_round(&FINAL_CHECK, round);
        }
        if round == 3 {
            let send_after = time_count() + time_frequency() / 100;
            while time_count() < send_after {
                core::hint::spin_loop();
            }
        }
        let mut signal = MINOR_SIGNAL
            .acquire(Option::take)
            .unwrap()
            .expect("admitted minor SGI");
        SIGNAL_WORK.store(round, Ordering::Release);
        sender.send_sgi(&mut signal).unwrap();
        assert!(sender.send_sgi(&mut signal).is_err());
        MINOR_SIGNAL
            .acquire(|slot| {
                assert!(slot.is_none());
                *slot = Some(signal);
            })
            .unwrap();
        SIGNAL_ISSUED.store(round, Ordering::Release);
        await_round(&SIGNAL_OBSERVED, round);
    }
    boot_diagnostic::write(
        "PASS arm-sgi target=1 irq=3 claim=3 eoi=3 restored=3 retired=3 idle=3\n",
    );
    sender
        .release()
        .unwrap_or_else(|_| panic!("major SGI interface busy"))
}

struct TimerRoute<'local, 'clock, 'tree> {
    receiver: &'local mut CpuInterface<'static>,
    timer: &'local mut VirtualTimer<'clock, 'tree, 'static>,
}

struct LocalTimerRoute {
    active: AtomicPtr<()>,
    returned: AtomicBool,
}

static TIMER_ROUTE: Binding<LocalTimerRoute> = Binding::new(
    LocalTimerRoute {
        active: AtomicPtr::new(ptr::null_mut()),
        returned: AtomicBool::new(false),
    },
    handle_timer,
);
static MAJOR_VECTORS: RuntimeTrap = RuntimeTrap::new(TIMER_ROUTE.entry());

fn handle_timer(owner: &LocalTimerRoute, interrupt: Irq<'_>) {
    assert_eq!(IRQ_TURN.acquire(|_| ()), Err(TurnAcquireError::Context));
    let pointer = owner
        .active
        .load(Ordering::Acquire)
        .cast::<TimerRoute<'_, '_, '_>>();
    // Only the major processor enters here, while receive_timer exclusively
    // transfers the fixture's two original owners to this handler.
    let route = unsafe { pointer.as_mut() }.expect("live timer route");
    let mut claim = route.receiver.receive().unwrap().expect("real timer PPI");
    route.timer.stop_in_interrupt(&interrupt, &claim).unwrap();
    claim
        .complete_ppi(route.timer.stopped_ppi().unwrap())
        .unwrap();
    owner.returned.store(true, Ordering::Release);
}

fn receive_timer(mut route: TimerRoute<'_, '_, '_>, until: u64) {
    let owner = TIMER_ROUTE.owner();
    assert!(
        owner
            .active
            .swap(ptr::from_mut(&mut route).cast(), Ordering::Release)
            .is_null()
    );
    let restored: usize;
    unsafe {
        asm!(
            "msr daifclr, #2",
            "isb",
            "2: ldarb w8, [{observed}]",
            "cbnz w8, 3f",
            "mrs x8, CNTVCT_EL0",
            "cmp x8, {until}",
            "b.lo 2b",
            "3: mrs {restored}, DAIF",
            "msr daifset, #2",
            "isb",
            observed = in(reg) owner.returned.as_ptr(),
            until = in(reg) until,
            restored = out(reg) restored,
            out("x8") _,
            options(nostack),
        );
    }
    owner.active.store(ptr::null_mut(), Ordering::Relaxed);
    assert_eq!(restored & 0x80, 0);
    assert!(owner.returned.swap(false, Ordering::Acquire));
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
    let distinct_counter = unsafe { counter.bind(major) }.unwrap();
    let counter = unsafe { counter.bind(major) }.unwrap();
    let clock = counter.clock();
    let mut captured = false;
    for delay in [Duration::from_secs(5), Duration::from_millis(10)] {
        receiver.prepare_ppi(&mut ppi).unwrap();
        let mut timer = unsafe { VirtualTimer::admit(timer, &counter, &mut ppi) }.unwrap();
        if delay == Duration::from_secs(5) {
            let foreign = distinct_counter.clock().sample().unwrap();
            assert_eq!(
                timer.arm_at(foreign.after(delay).unwrap()),
                Err(TimerFailure::Scope)
            );
        }
        timer_consumer::arm(clock, &mut timer, delay);
        if delay == Duration::from_secs(5) {
            assert_eq!(timer.stop(), Err(TimerFailure::Busy));
            timer_consumer::advance(clock, &mut timer, Duration::from_millis(10));
        }
        assert!(timer.stopped_ppi().is_none());
        if delay == Duration::from_millis(10) {
            unsafe { MAJOR_VECTORS.install() }.unwrap();
            let now: u64;
            unsafe { asm!("mrs {}, CNTVCT_EL0", out(reg) now, options(nostack, nomem)) };
            receive_timer(
                TimerRoute {
                    receiver: &mut receiver,
                    timer: &mut timer,
                },
                now + clock.frequency().get() * 3,
            );
            assert!(timer.stopped_ppi().is_none());
            assert_eq!(receiver.state(), State::Empty);
            captured = true;
            continue;
        }
        wait_for_claim(&mut receiver, role.intid as usize);
        timer_consumer::confirm_due(&timer);
        assert_eq!(timer.pending(), Ok(true));
        unsafe { timer_consumer::stop(&mut timer) }.unwrap();
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
    assert!(captured);
    let mut frame = receiver
        .release()
        .unwrap_or_else(|_| panic!("timer receiver remains live"));
    for _ in 0..4096 {
        match unsafe { frame.retire_ppi(ppi) } {
            Ok(()) => {
                boot_diagnostic::write(
                    "PASS arm-virtual-timer programmed=2 ppi=2 eoi=2 runtime=1 restored=1 retired=1\n",
                );
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
    let mut prior_receipt: Option<ClaimId> = None;
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
        let (mut primary_binding, mut secondary_binding) = unsafe {
            let mut primary_binding = spi.reserve(route).unwrap();
            let mut secondary_binding = spi.reserve(second_route).unwrap();
            assert!(spi.reserve(second_route).is_err());
            route_consumer::publish(
                &mut spi
                    .configure(&mut primary_binding, receiver.destination().unwrap(), 0x10)
                    .unwrap(),
            );
            assert_eq!(spi.state(second_id), Ok(SpiState::Masked));
            route_consumer::publish(
                &mut spi
                    .configure(
                        &mut secondary_binding,
                        receiver.destination().unwrap(),
                        0x20,
                    )
                    .unwrap(),
            );
            (primary_binding, secondary_binding)
        };
        for (input, uart, binding) in [
            (id, &mut console, &mut primary_binding),
            (second_id, &mut secondary, &mut secondary_binding),
        ] {
            assert_eq!(spi.state(input), Ok(SpiState::Enabled));
            transmit(uart);
            let receipt = wait_for_claim(&mut receiver, input);
            if let Some(prior) = prior_receipt {
                assert_eq!(receipt.controller(), prior.controller());
                assert_eq!(receipt.context(), prior.context());
                assert!(receipt.round() > prior.round());
            }
            prior_receipt = Some(receipt);
            assert_eq!(receiver.state(), State::Claimed(input as u32));
            let mut claim = claim_consumer::pending(&mut receiver, receipt);
            assert_eq!(claim.intid(), input as u32);
            assert!(unsafe { claim.deactivate() }.is_err());
            assert_eq!(unsafe { uart.read_u32(UART_STATUS) }.unwrap(), TX_INTERRUPT);
            clear_transmit(uart);
            route_consumer::mask(&mut spi.control(binding).unwrap());
            assert_eq!(spi.state(input), Ok(SpiState::Quiesced));
            assert_eq!(spi.active(input), Ok(true));
            if input == id {
                assert_eq!(spi.state(second_id), Ok(SpiState::Enabled));
            }
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
            unsafe { route_consumer::resume(&mut spi.control(binding).unwrap()) };
            assert_eq!(spi.state(input), Ok(SpiState::Enabled));
            route_consumer::mask(&mut spi.control(binding).unwrap());
            let request = unsafe { spi.begin_retirement(binding) }.unwrap();
            let receipt = receiver.observe_retirement(request).unwrap();
            unsafe { spi.finish_retirement(binding, receipt) }.unwrap();
            let mut replacement =
                unsafe { spi.reserve(if input == id { route } else { second_route }) }.unwrap();
            assert!(spi.control(binding).is_err());
            spi.cancel(&mut replacement).unwrap();
            assert!(spi.cancel(&mut replacement).is_err());
        }
        local_frame = receiver
            .release()
            .unwrap_or_else(|_| panic!("original GIC reception remains live"));
    }
    local_frame = check_virtual_timer(tree, controller, major, local_frame);
    local_frame = check_signal(local_frame);
    assert!(local_frame.awake());
    let remote = unsafe { CpuInterface::admit(local_frame) }
        .unwrap_or_else(|(_, reason)| panic!("major GIC interface: {reason:?}"));
    spi::close_remote(
        controller,
        distributor.reborrow(),
        second_route,
        &mut secondary,
        &remote,
        minor,
    );
    assert!(remote.release().is_ok());
    boot_diagnostic::write("PASS gicr frames=2 independent=1\n");
    boot_diagnostic::write("PASS external-route published=2 repeated=0 independent=1\n");
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
    boot_diagnostic::console()
        .unwrap()
        .write_str("PASS virt interrupt uart-primary=2 uart-secondary=2 shared-words=1 iar=4 eoi=4 dir=2 masked=1\n")
        .unwrap();
}
