//! UEFI loader input and memory descriptor format.

mod loader;
pub mod map;

pub use loader::{AcpiInput, DeviceTree, Loader};
