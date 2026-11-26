//! Actual firmware protocols and loader inputs.

pub mod psci;
#[cfg(any(test, limen_firmware = "sbi"))]
pub mod sbi;
pub mod uefi;
