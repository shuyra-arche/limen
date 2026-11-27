//! Formatting for a byte-oriented early diagnostic channel.
use core::fmt::{self, Write};

/// A writer backed by the output operation supplied by the boot environment.
pub struct Console {
    pub(crate) write_byte: fn(u8) -> bool,
}

impl Console {
    pub const fn new(write_byte: fn(u8) -> bool) -> Self {
        Self { write_byte }
    }
}

impl Write for Console {
    fn write_str(&mut self, message: &str) -> fmt::Result {
        for byte in message.bytes() {
            if !(self.write_byte)(byte) {
                return Err(fmt::Error);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "../../../../tests/metal/platform/boot/console.rs"]
mod tests;
