//! UEFI loader input and memory descriptor format.

#[cfg(any(limen_firmware = "uefi", test))]
mod loader;
pub mod map;

#[cfg(any(limen_firmware = "uefi", test))]
pub use loader::{AcpiInput, DeviceTree, Loader};
