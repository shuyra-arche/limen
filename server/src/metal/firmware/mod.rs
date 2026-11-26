#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
pub mod acpi;
#[cfg(target_arch = "aarch64")]
pub mod arm;
pub mod devicetree;
pub(crate) mod map;
#[cfg(target_arch = "aarch64")]
pub(crate) mod psci;
#[cfg(target_arch = "riscv64")]
pub(crate) mod sbi;
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
pub(crate) mod uefi;
