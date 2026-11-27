//! Actual firmware protocols and loader inputs.

pub(crate) mod psci;
#[cfg(any(test, limen_firmware = "sbi"))]
pub(crate) mod sbi;
pub mod uefi;

pub mod treaty;
