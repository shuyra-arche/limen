//! Common startup admission and launch with the selected machine entry.

pub(crate) mod console;
pub(crate) mod handoff;
#[cfg(limen_target)]
pub(crate) mod major;
#[cfg(limen_target)]
pub(crate) mod minor;
#[cfg(limen_target)]
pub(crate) mod services;
#[cfg(limen_target)]
pub(in crate::metal) mod trap;
#[cfg(limen_target)]
pub(crate) use trap::stack_range as trap_stack_range;

#[cfg(all(limen_target, target_arch = "aarch64"))]
mod aarch64;
#[cfg(all(limen_target, target_arch = "riscv64"))]
mod riscv;
#[cfg(all(limen_target, target_arch = "x86_64"))]
mod x86_64;

#[cfg(all(limen_target, target_arch = "aarch64"))]
use self::aarch64 as backend;
#[cfg(all(limen_target, target_arch = "riscv64"))]
use self::riscv as backend;
#[cfg(all(limen_target, target_arch = "x86_64"))]
use self::x86_64 as backend;

#[cfg(limen_firmware = "sbi")]
use handoff::sbi as firmware;
#[cfg(limen_firmware = "uefi")]
use handoff::uefi as firmware;

#[cfg(limen_target)]
pub use backend::Startup;
#[cfg(limen_firmware = "uefi")]
pub(crate) use backend::enter_major;
#[cfg(limen_firmware = "sbi")]
pub(crate) use backend::image;
#[cfg(limen_target)]
pub(in crate::metal) use backend::entry;
#[cfg(limen_target)]
pub(crate) use backend::{
    DeliveryError, MinorContext, admit, established, minor_context, park, wake_minor,
};

/// Major and minor CPUs use the same ordinary startup stack budget.
#[cfg(limen_target)]
pub(in crate::metal) const STACK_BYTES: usize = 32 * 1024;
