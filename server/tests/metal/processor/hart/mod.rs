//! Host inputs for production Hart-local synchronization.

use super::{HartLocal, Storage};
use crate::metal::sync::TURN_CAPACITY;
use core::{cell::{Cell, OnceCell}, ptr};
use std::boxed::Box;

std::thread_local! {
    static LOCAL: OnceCell<&'static HartLocal> = const { OnceCell::new() };
    static MASKED: Cell<bool> = const { Cell::new(false) };
}

pub(super) fn local_address() -> usize {
    LOCAL.with(|local| {
        let hart = local.get_or_init(|| unsafe { Box::leak(Box::new(Storage::<TURN_CAPACITY>::new())).bind() });
        ptr::from_ref(*hart).expose_provenance()
    })
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
        processor::hart::local::can_switch,
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

#[test]
fn acquisition_uses_the_nodes_supplied_to_this_hart() {
    use crate::metal::sync::{TurnAcquireError, TurnLock, TurnMode, TurnWorkspace, turn_capacity};

    std::thread::spawn(|| {
        let hart = unsafe { Box::leak(Box::new(Storage::<1>::new())).bind() };
        LOCAL.with(|local| assert!(local.set(hart).is_ok()));
        assert_eq!(turn_capacity(), Some(1));
        let first = TurnLock::new(0);
        let second = TurnLock::new(0);
        let mut workspace = TurnWorkspace::<2>::new();
        let mut plan = workspace.plan();
        plan.insert(&first).unwrap();
        plan.insert(&second).unwrap();
        assert_eq!(plan.acquire(TurnMode::Slow, |_| ()), Err(TurnAcquireError::Capacity));
        assert_eq!(first.acquire(|value| *value), Ok(0));
    })
    .join()
    .unwrap();
}
