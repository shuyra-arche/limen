//! Installed local execution environments and their native controls.

pub(crate) mod aarch64;
#[cfg(limen_target)]
pub(in crate::metal) mod entry;
#[cfg(limen_target)]
pub(in crate::metal::processor) mod launch;
pub(crate) mod local;
pub(crate) mod riscv64;
pub(crate) mod x86_64;
