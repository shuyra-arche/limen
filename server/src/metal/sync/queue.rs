use core::{
    hint::spin_loop,
    ptr::{self, null_mut},
    sync::atomic::{
        AtomicPtr,
        Ordering::{AcqRel, Acquire, Relaxed, Release},
    },
};

use crate::metal::hart::QueueNode;

pub(super) struct Queue {
    tail: AtomicPtr<QueueNode>,
}

impl Queue {
    pub(super) const fn new() -> Self {
        Self {
            tail: AtomicPtr::new(null_mut()),
        }
    }

    pub(super) fn acquire(&self, node: &QueueNode) {
        let current = ptr::from_ref(node).cast_mut();
        let previous = self.tail.swap(current, AcqRel);
        if !previous.is_null() {
            // The predecessor stays in fixed storage and cannot reuse its
            // node until this link has been published.
            unsafe { (*previous).next.store(current, Release) };
            while node.waiting.load(Acquire) {
                spin_loop();
            }
        }
    }

    pub(super) fn attempt(&self, node: &QueueNode) -> bool {
        self.tail
            .compare_exchange(null_mut(), ptr::from_ref(node).cast_mut(), AcqRel, Relaxed)
            .is_ok()
    }

    pub(super) fn release(&self, node: &QueueNode) {
        let current = ptr::from_ref(node).cast_mut();
        let mut successor = node.next.load(Acquire);
        if successor.is_null() {
            if self
                .tail
                .compare_exchange(current, null_mut(), Release, Relaxed)
                .is_ok()
            {
                return;
            }
            // A successor has exchanged the tail. It can link while the
            // outgoing Hart waits, since linking takes no other lock.
            loop {
                successor = node.next.load(Acquire);
                if !successor.is_null() {
                    break;
                }
                spin_loop();
            }
        }
        // The successor cannot leave or reuse its node before taking the baton.
        unsafe { (*successor).waiting.store(false, Release) };
    }
}
