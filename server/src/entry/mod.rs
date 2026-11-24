#[cfg_attr(not(target_arch = "riscv64"), path = "boot/early.rs")]
pub mod boot;
#[cfg(target_arch = "riscv64")]
pub mod execution;
