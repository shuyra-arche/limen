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
    // The guest exposes its original Timer only during `receive`; masked
    // ordinary code takes the pointer down before reborrowing it.
    let pointer = owner.active.load(Ordering::Acquire).cast::<Timer<'_>>();
    let timer = unsafe { pointer.as_mut() }.expect("live local timer");
    if unsafe { timer.accept_interrupt(&interrupt) }.unwrap() {
        owner.captured.store(true, Ordering::Release);
    }
}

pub(crate) fn install() {
    unsafe { VECTORS.install() }.unwrap();
}

pub(crate) fn receive(timer: &mut Timer<'_>, until: u64) {
    let owner = RECEPTION.owner();
    assert!(
        owner
            .active
            .swap(ptr::from_mut(timer).cast(), Ordering::Release)
            .is_null()
    );
    unsafe {
        asm!(
            "csrsi sstatus, 2",
            "2: lbu t0, 0({captured})",
            "bnez t0, 3f",
            "rdtime t1",
            "bltu t1, {until}, 2b",
            "3: csrci sstatus, 2",
            until = in(reg) until,
            captured = in(reg) owner.captured.as_ptr(),
            out("t0") _,
            out("t1") _,
            options(nostack),
        );
    }
    owner.active.store(ptr::null_mut(), Ordering::Relaxed);
    assert!(owner.captured.swap(false, Ordering::Acquire));
}
