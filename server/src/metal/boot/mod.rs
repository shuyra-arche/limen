//! Common startup admission and launch with the selected machine entry.

pub(crate) mod console;
pub(crate) mod major;
pub(crate) mod minor;
pub(crate) mod services;
pub(in crate::metal) mod trap;

#[cfg(target_arch = "riscv64")]
#[path = "riscv/mod.rs"]
mod backend;
#[cfg(target_arch = "x86_64")]
#[path = "x86_64/mod.rs"]
mod backend;
#[cfg(target_arch = "aarch64")]
#[path = "aarch64/mod.rs"]
mod backend;

#[cfg(target_arch = "riscv64")]
use super::firmware::sbi as firmware;
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
use super::firmware::uefi as firmware;

pub use backend::Startup;
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
pub(crate) use backend::enter_major;
#[cfg(target_arch = "riscv64")]
pub(crate) use backend::image;
pub(crate) use backend::{
    DeliveryError, MinorContext, established, minor_context, park, wake_minor,
};

/// Major and minor CPUs use the same ordinary startup stack budget.
pub(in crate::metal) const STACK_BYTES: usize = 32 * 1024;
