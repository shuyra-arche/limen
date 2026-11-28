//! Native acknowledgment, comparator stop, and completion on one PE.

use core::{
    arch::asm,
    ptr,
    sync::atomic::{AtomicBool, AtomicPtr, Ordering},
};
use kernel::{
    entry::hook::Binding,
    metal::{
        interrupt::aarch64::{gic::cpu_interface::CpuInterface, timer::VirtualTimer},
        processor::treaty::{Irq, RuntimeTrap},
        sync::treaty::{TurnAcquireError, TurnLock},
    },
};

static IRQ_TURN: TurnLock<()> = TurnLock::new(());

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
static VECTORS: RuntimeTrap = RuntimeTrap::new(TIMER_ROUTE.entry());

pub fn install() {
    unsafe { VECTORS.install() }.expect("permanent virtual timer entry");
}

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

pub fn receive(
    receiver: &mut CpuInterface<'static>,
    timer: &mut VirtualTimer<'_, '_, 'static>,
    until: u64,
) {
    let mut route = TimerRoute { receiver, timer };
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
