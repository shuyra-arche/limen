use core::sync::atomic::{AtomicBool, AtomicPtr};

/// An MCS node resides in fixed Hart storage from admission until detachment.
pub(in crate::metal::sync) struct QueueNode {
    pub(super) next: AtomicPtr<QueueNode>,
    pub(super) waiting: AtomicBool,
}

impl QueueNode {
    #[cfg_attr(
        not(any(limen_target, test)),
        expect(dead_code, reason = "the CPU startup supply constructs resident nodes")
    )]
    pub(in crate::metal::sync) const fn new() -> Self {
        Self {
            next: AtomicPtr::new(core::ptr::null_mut()),
            waiting: AtomicBool::new(false),
        }
    }
}
