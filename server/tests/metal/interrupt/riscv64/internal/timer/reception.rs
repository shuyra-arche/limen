//! Exercise the real supervisor timer path using the original comparator owner.

use core::{
    arch::asm,
    ptr,
    sync::atomic::{AtomicBool, AtomicPtr, Ordering},
};
use kernel::entry::hook::Binding;
use kernel::metal::{
    interrupt::riscv64::internal::timer::Timer,
    processor::treaty::{RuntimeTrap, TimerInterrupt},
};

type AfterReceipt = for<'frame> fn(TimerInterrupt<'frame>);

struct Active<'timer> {
    timer: *mut Timer<'timer>,
    after: Option<AfterReceipt>,
}

struct TimerReception {
    active: AtomicPtr<()>,
    captured: AtomicBool,
}

static RECEPTION: Binding<TimerReception> = Binding::new(
    TimerReception {
        active: AtomicPtr::new(ptr::null_mut()),
        captured: AtomicBool::new(false),
    },
    None,
    Some(interrupt),
);
static VECTORS: RuntimeTrap = RuntimeTrap::new(RECEPTION.entry());

fn interrupt(owner: &TimerReception, interrupt: TimerInterrupt<'_>) {
    // The dispatcher keeps its original Timer and optional callback live
    // under this one pointer until the caller has masked interrupts again.
    let pointer = owner.active.load(Ordering::Acquire).cast::<Active<'_>>();
    let active = unsafe { pointer.as_ref() }.expect("live local timer");
    if unsafe { (&mut *active.timer).accept_interrupt(&interrupt) }.unwrap() {
        owner.captured.store(true, Ordering::Release);
        if let Some(after) = active.after {
            after(interrupt);
        }
    }
}

pub(crate) fn install() {
    unsafe { VECTORS.install() }.unwrap();
}

pub(crate) fn receive(timer: &mut Timer<'_>, until: u64) {
    capture(timer, None, |captured| unsafe {
        asm!(
            "csrsi sstatus, 2",
            "2: lbu t0, 0({captured})",
            "bnez t0, 3f",
            "rdtime t1",
            "bltu t1, {until}, 2b",
            "3: csrci sstatus, 2",
            until = in(reg) until,
            captured = in(reg) captured.as_ptr(),
            out("t0") _,
            out("t1") _,
            options(nostack),
        );
    });
}

pub(crate) fn capture<T>(
    timer: &mut Timer<'_>,
    after: Option<AfterReceipt>,
    run: impl FnOnce(&AtomicBool) -> T,
) -> T {
    let owner = RECEPTION.owner();
    let active = Active {
        timer: ptr::from_mut(timer),
        after,
    };
    assert!(
        owner
            .active
            .swap(ptr::from_ref(&active).cast_mut().cast(), Ordering::Release)
            .is_null()
    );
    let result = run(&owner.captured);
    owner.active.store(ptr::null_mut(), Ordering::Relaxed);
    assert!(owner.captured.swap(false, Ordering::Acquire));
    result
}
