//! Early operations supplied by firmware or the trusted image.
use super::console::Console;
use crate::metal::platform::boot;
use core::{
    fmt::{self, Write},
    ptr,
    sync::atomic::{AtomicPtr, Ordering},
};

#[derive(Clone, Copy)]
pub struct Services {
    pub output: Option<fn(u8) -> bool>,
    pub reset: Option<fn(bool)>,
}

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

    pub(crate) fn console(&self) -> Option<Console> {
        self.output.map(Console::new)
    }

    pub(crate) fn reject(&self, reason: fmt::Arguments<'_>) -> ! {
        if let Some(mut console) = self.console() {
            let _ = console.write_fmt(reason);
        }
        self.shutdown(false)
    }

    pub(crate) fn shutdown(&self, success: bool) -> ! {
        if let Some(reset) = self.reset {
            reset(success);
        }
        boot::park()
    }
}
