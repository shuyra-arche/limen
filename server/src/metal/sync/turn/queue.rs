use core::{
    hint::spin_loop,
    ptr::{self, NonNull, null_mut},
    sync::atomic::{
        AtomicPtr,
        Ordering::{AcqRel, Acquire, Relaxed, Release},
    },
};

use crate::metal::processor::hart::local::QueueNode;

#[cfg(test)]
#[path = "../../../../tests/metal/sync/turn/queue.rs"]
mod tests;

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
        self.departure(node).finish();
    }

    /// A strong clear-tail CAS either detaches this node or proves that a
    /// successor already owns the queue tail. The link can arrive later.
    fn departure<'a>(&self, node: &'a QueueNode) -> Departure<'a> {
        if let Some(successor) = NonNull::new(node.next.load(Acquire)) {
            return Departure::Linked(successor);
        }
        let current = ptr::from_ref(node).cast_mut();
        if self
            .tail
            .compare_exchange(current, null_mut(), Release, Relaxed)
            .is_ok()
        {
            Departure::Detached
        } else {
            Departure::AwaitLink(node)
        }
    }
}

// This is the local result of one real release decision, not a retained
// queue state. Only the original release path can finish the decision.
enum Departure<'a> {
    Detached,
    Linked(NonNull<QueueNode>),
    AwaitLink(&'a QueueNode),
}

impl Departure<'_> {
    fn finish(self) {
        let successor = match self {
            Self::Detached => return,
            Self::Linked(successor) => successor,
            Self::AwaitLink(node) => loop {
                if let Some(successor) = NonNull::new(node.next.load(Acquire)) {
                    break successor;
                }
                spin_loop();
            },
        };
        // The successor cannot leave or reuse its node before taking the baton.
        unsafe { successor.as_ref().waiting.store(false, Release) };
    }
}
