//! Physical Hart descriptions and the local context installed at machine entry.

pub mod aarch64;
pub mod riscv;
#[cfg(any(limen_target, test))]
pub(crate) mod local;
#[cfg(all(limen_target, target_arch = "x86_64"))]
pub(in crate::metal) mod x86_64;
