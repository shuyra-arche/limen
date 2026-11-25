//! Trusted local CPU facts and fixed stack storage.
pub(in crate::metal) mod stack;

#[cfg(target_arch = "riscv64")]
#[path = "riscv.rs"]
mod backend;
#[cfg(target_arch = "x86_64")]
#[path = "x86_64.rs"]
mod backend;
#[cfg(target_arch = "aarch64")]
#[path = "aarch64.rs"]
mod backend;

pub(crate) use backend::{current_hart, stack_pointer};
