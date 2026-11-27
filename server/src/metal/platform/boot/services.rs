//! Early operations supplied by firmware or the trusted image.
use super::treaty::Services;
use crate::metal::processor::treaty::Terminal;
use core::{
    fmt, ptr,
    sync::atomic::{AtomicPtr, Ordering},
};

// Before the handoff a diagnostic has no output and shutdown parks the CPU.
static EMPTY: Services = Services {
    output: None,
    reset: None,
};
static ACTIVE: AtomicPtr<Services> = AtomicPtr::new(ptr::from_ref(&EMPTY).cast_mut());

impl Services {
    pub(crate) fn install(&'static self) {
        ACTIVE.store(ptr::from_ref(self).cast_mut(), Ordering::Release);
    }

    pub(crate) fn active() -> &'static Self {
        // Both EMPTY and every table accepted by install are immutable statics.
        unsafe { &*ACTIVE.load(Ordering::Acquire) }
    }

    pub(crate) const fn terminal(&self) -> Terminal {
        Terminal::new(self.output, self.reset)
    }

    pub(crate) fn reject(&self, reason: fmt::Arguments<'_>) -> ! {
        self.terminal().reject(reason)
    }
}
