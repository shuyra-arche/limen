//! Host inputs for production Hart-local synchronization.

use super::HartLocal;
use core::{cell::Cell, ptr};
use std::boxed::Box;

std::thread_local! {
    static LOCAL: &'static HartLocal = Box::leak(Box::new(HartLocal::new()));
    static MASKED: Cell<bool> = const { Cell::new(false) };
}

pub(super) fn ordinary_entry() -> bool {
    true
}

pub(super) fn local_address() -> usize {
    LOCAL.with(|local| ptr::from_ref(*local).expose_provenance())
}

pub(super) unsafe fn mask_interrupts() -> usize {
    MASKED.with(|masked| usize::from(masked.replace(true)))
}

pub(super) unsafe fn restore_interrupts(saved: usize) {
    MASKED.with(|masked| {
        assert!(masked.replace(saved != 0));
    });
}

#[test]
fn cooperative_switch_requires_no_local_participation() {
    use crate::metal::{
        hart::local::can_switch,
        sync::{LeafLock, TurnLock},
    };

    assert!(can_switch());
    let turn = TurnLock::new(());
    turn.acquire(|_| assert!(!can_switch())).unwrap();
    assert!(can_switch());

    let leaf = LeafLock::new(());
    leaf.attempt(|_| assert!(!can_switch())).unwrap();
    assert!(can_switch());
}
