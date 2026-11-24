//! Early output and termination available under the selected platform contract.
use super::boot;
use core::fmt::{self, Write};

/// A writer backed by the early console supplied by this platform.
pub struct Console(fn(u8) -> bool);

impl Write for Console {
    fn write_str(&mut self, message: &str) -> fmt::Result {
        if message.bytes().all(self.0) {
            Ok(())
        } else {
            Err(fmt::Error)
        }
    }
}

#[derive(Clone, Copy)]
pub struct Services {
    pub output: Option<fn(u8) -> bool>,
    pub reset: Option<fn(bool)>,
}

impl Services {
    pub(crate) fn reject(&self, reason: fmt::Arguments<'_>) -> ! {
        if let Some(output) = self.output {
            let _ = Console(output).write_fmt(reason);
        }
        self.shutdown(false)
    }

    pub(crate) fn console(&self) -> Option<Console> {
        self.output.map(Console)
    }

    pub(crate) fn shutdown(&self, success: bool) -> ! {
        if let Some(reset) = self.reset {
            reset(success);
        }
        boot::park()
    }
}

#[cfg(not(target_arch = "riscv64"))]
static ACTIVE: core::sync::atomic::AtomicPtr<Services> =
    core::sync::atomic::AtomicPtr::new(core::ptr::null_mut());

#[cfg(not(target_arch = "riscv64"))]
pub(crate) fn install(services: &'static Services) {
    ACTIVE.store(
        core::ptr::from_ref(services).cast_mut(),
        core::sync::atomic::Ordering::Release,
    );
}

#[cfg(not(target_arch = "riscv64"))]
pub(crate) fn services() -> Services {
    let ptr = ACTIVE.load(core::sync::atomic::Ordering::Acquire);
    // The major publishes an immutable, image-owned static before switching stacks.
    unsafe { ptr.as_ref() }.copied().unwrap_or(Services {
        output: None,
        reset: None,
    })
}
#[cfg(target_arch = "riscv64")]
pub(crate) use crate::metal::riscv::firmware::sbi::services;

/// Obtain a writer only if this platform supplies a diagnostic device.
pub fn console() -> Option<Console> {
    services().console()
}

/// Best-effort output for trusted early boot code.
pub fn write(message: &str) {
    if let Some(mut console) = console() {
        let _ = console.write_str(message);
    }
}

/// Request platform termination and park if no reset is available or it returns.
pub fn shutdown(success: bool) -> ! {
    services().shutdown(success)
}
