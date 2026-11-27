//! Actual control-flow transfer and machine execution descriptions.

#[cfg(limen_cooperative)]
pub(super) mod cooperative;

#[cfg(all(limen_target, any(target_arch = "aarch64", target_arch = "riscv64")))]
pub(super) mod runtime;

pub(crate) mod terminal;

mod profile;
