#[cfg(target_arch = "aarch64")]
pub(crate) mod aarch64;
pub(crate) mod console;
pub(crate) mod firmware;
#[cfg(target_arch = "riscv64")]
pub(crate) mod riscv;
pub(crate) mod stack;
#[cfg(target_arch = "x86_64")]
pub(crate) mod x86_64;

#[cfg(target_arch = "aarch64")]
pub(crate) use aarch64 as arch;
#[cfg(target_arch = "riscv64")]
pub(crate) use riscv as arch;
#[cfg(target_arch = "x86_64")]
pub(crate) use x86_64 as arch;

pub(crate) use arch::boot;

pub(crate) mod minor;
