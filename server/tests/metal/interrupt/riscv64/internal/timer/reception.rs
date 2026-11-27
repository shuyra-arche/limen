//! Exercise the real supervisor timer path using the original comparator owner.

use core::{
    arch::asm,
    ptr,
    sync::atomic::{AtomicBool, AtomicPtr, Ordering},
};
use kernel::metal::{
    interrupt::riscv64::internal::timer::Timer,
    processor::treaty::{RuntimeTrap, TimerInterrupt},
};

static VECTORS: RuntimeTrap = RuntimeTrap::new(None, Some(interrupt));
static ACTIVE: AtomicPtr<()> = AtomicPtr::new(ptr::null_mut());
static CAPTURED: AtomicBool = AtomicBool::new(false);

fn interrupt(interrupt: TimerInterrupt<'_>) {
    // The guest exposes its original Timer only during `receive`; masked
    // ordinary code takes the pointer down before reborrowing it.
    let pointer = ACTIVE.load(Ordering::Acquire).cast::<Timer<'_>>();
    let timer = unsafe { pointer.as_mut() }.expect("live local timer");
    if unsafe { timer.accept_interrupt(&interrupt) }.unwrap() {
        CAPTURED.store(true, Ordering::Release);
    }
}

pub(crate) fn install() {
    unsafe { VECTORS.install() }.unwrap();
}

pub(crate) fn receive(timer: &mut Timer<'_>, until: u64) {
    assert!(
        ACTIVE
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
            captured = in(reg) CAPTURED.as_ptr(),
            out("t0") _,
            out("t1") _,
            options(nostack),
        );
    }
    ACTIVE.store(ptr::null_mut(), Ordering::Relaxed);
    assert!(CAPTURED.swap(false, Ordering::Acquire));
}
