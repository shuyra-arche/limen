//! Common startup admission and launch with the selected machine entry.

pub(crate) mod handoff;
#[cfg(limen_target)]
pub(crate) mod major;
#[cfg(limen_target)]
pub(crate) mod services;
#[cfg(limen_target)]
pub(in crate::metal) mod trap;
pub mod treaty;
#[cfg(limen_target)]
pub(crate) use trap::stack_range as trap_stack_range;

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

#[cfg(limen_firmware = "uefi")]
pub(crate) use backend::enter_major;
#[cfg(limen_target)]
pub(in crate::metal) use backend::entry;
#[cfg(limen_firmware = "sbi")]
pub(crate) use backend::image;
#[cfg(limen_target)]
pub(crate) use backend::{admit, established};

/// Major and minor CPUs use the same ordinary startup stack budget.
#[cfg_attr(
    not(limen_target),
    expect(dead_code, reason = "only native boot allocates the startup stacks")
)]
pub(in crate::metal) const STACK_BYTES: usize = 32 * 1024;
