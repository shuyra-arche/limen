pub mod acpi;
#[cfg(all(limen_target, target_arch = "aarch64"))]
pub mod arm;
pub mod devicetree;
#[cfg(any(limen_target, test))]
pub(crate) mod facts;
pub mod map;
#[cfg(all(limen_target, target_arch = "aarch64"))]
pub mod psci;
#[cfg(any(test, limen_firmware = "sbi"))]
pub mod sbi;
#[cfg(any(test, limen_firmware = "uefi"))]
pub mod uefi;
