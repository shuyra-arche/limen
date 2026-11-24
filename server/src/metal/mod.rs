pub(crate) mod generic;

#[cfg(target_arch = "x86_64")]
#[path = "x86_64/mod.rs"]
mod arch;
#[cfg(target_arch = "aarch64")]
#[path = "aarch64/mod.rs"]
mod arch;
#[cfg(target_arch = "riscv64")]
#[path = "riscv/mod.rs"]
mod arch;

pub(crate) use arch::boot;
#[cfg(target_arch = "riscv64")]
pub(crate) use arch::execution;
