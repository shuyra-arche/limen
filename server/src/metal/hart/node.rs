use core::{
    ptr,
    sync::atomic::{AtomicBool, AtomicPtr},
};

/// An MCS node resides in fixed Hart storage from admission until detachment.
pub(crate) struct QueueNode {
    pub(crate) next: AtomicPtr<QueueNode>,
    pub(crate) waiting: AtomicBool,
}

impl QueueNode {
    pub(crate) const fn new() -> Self {
        Self {
            next: AtomicPtr::new(ptr::null_mut()),
            waiting: AtomicBool::new(false),
        }
    }
}
