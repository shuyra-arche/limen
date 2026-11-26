//! SBI firmware capability discovery and native calls.

mod probe;

#[cfg(limen_firmware = "sbi")]
mod transport;
#[cfg(limen_firmware = "sbi")]
pub use probe::SbiServices;
#[cfg(limen_firmware = "sbi")]
pub use transport::platform_services;
#[cfg(limen_firmware = "sbi")]
pub(crate) use transport::{admit, services_on_entry, start_hart, trap_services};
