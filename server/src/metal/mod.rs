pub(crate) mod generic;

#[cfg(target_arch = "x86_64")]
#[path = "x86_64/mod.rs"]
pub(crate) mod arch;
#[cfg(target_arch = "aarch64")]
#[path = "aarch64/mod.rs"]
pub(crate) mod arch;
#[cfg(target_arch = "riscv64")]
#[path = "riscv/mod.rs"]
pub(crate) mod arch;

pub(crate) use arch::boot;
