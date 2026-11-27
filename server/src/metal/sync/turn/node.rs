use core::sync::atomic::{AtomicBool, AtomicPtr};

/// An MCS node resides in fixed processor storage from admission until detachment.
pub(in crate::metal::sync) struct QueueNode {
    pub(super) next: AtomicPtr<QueueNode>,
    pub(super) waiting: AtomicBool,
}

impl QueueNode {
    pub(in crate::metal::sync) const fn new() -> Self {
        Self {
            next: AtomicPtr::new(core::ptr::null_mut()),
            waiting: AtomicBool::new(false),
        }
    }
}
