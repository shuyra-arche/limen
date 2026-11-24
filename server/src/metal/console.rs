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

pub(crate) struct Services {
    pub(crate) output: Option<fn(u8) -> bool>,
    pub(crate) reset: Option<fn(bool)>,
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
