pub mod acpi;
pub mod devicetree;
#[cfg(any(limen_target, test))]
pub(crate) mod facts;
pub mod map;
pub mod psci;
#[cfg(any(test, limen_firmware = "sbi"))]
pub mod sbi;
#[cfg(any(test, limen_firmware = "uefi"))]
pub mod uefi;
