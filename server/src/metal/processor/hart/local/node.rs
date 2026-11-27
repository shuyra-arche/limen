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
    #[cfg_attr(
        not(any(limen_target, test)),
        expect(
            dead_code,
            reason = "CPU-local construction supplies the resident nodes"
        )
    )]
    pub(crate) const fn new() -> Self {
        Self {
            next: AtomicPtr::new(ptr::null_mut()),
            waiting: AtomicBool::new(false),
        }
    }
}
