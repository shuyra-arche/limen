#[cfg(target_arch = "aarch64")]
pub(crate) mod psci;
#[cfg(target_arch = "riscv64")]
pub(crate) mod sbi;
pub(crate) mod uefi;
