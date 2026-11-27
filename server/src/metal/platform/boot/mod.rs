//! Common startup admission and launch with the selected machine entry.

pub(crate) mod handoff;
#[cfg(limen_target)]
mod major;
pub mod treaty;

#[cfg(all(limen_target, target_arch = "aarch64"))]
pub(in crate::metal) mod aarch64;
#[cfg(all(limen_target, target_arch = "riscv64"))]
pub(in crate::metal) mod riscv64;
#[cfg(all(limen_target, target_arch = "x86_64"))]
pub(in crate::metal) mod x86_64;

#[cfg(all(limen_target, target_arch = "aarch64"))]
use self::aarch64 as backend;
#[cfg(all(limen_target, target_arch = "riscv64"))]
use self::riscv64 as backend;
#[cfg(all(limen_target, target_arch = "x86_64"))]
use self::x86_64 as backend;

#[cfg(limen_firmware = "sbi")]
use handoff::sbi as firmware;
#[cfg(limen_firmware = "uefi")]
use handoff::uefi as firmware;

#[cfg(limen_target)]
pub(crate) use backend::admit;
#[cfg(limen_firmware = "uefi")]
pub(crate) use backend::enter_major;
#[cfg(limen_firmware = "sbi")]
pub(crate) use backend::{image, linked_stack};
