//! Physical Hart descriptions and the local context installed at machine entry.

pub mod aarch64;
#[cfg(any(limen_target, test))]
pub(crate) mod local;
pub mod riscv;
pub mod x86_64;
