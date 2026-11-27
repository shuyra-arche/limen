//! UEFI loader input and memory descriptor format.

mod loader;
pub(crate) mod map;

pub use loader::{AcpiInput, DeviceTree, Loader};
