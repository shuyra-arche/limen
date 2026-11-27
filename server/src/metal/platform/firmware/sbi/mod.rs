//! Native SBI transport and shared probing rules.

pub(in crate::metal::platform::firmware) mod probe;
#[cfg(limen_firmware = "sbi")]
pub(in crate::metal::platform::firmware) mod transport;

#[cfg(limen_firmware = "sbi")]
pub(crate) use transport::{extensions, reset, write_byte};
