//! Ordinary machine entry checks shared by major and minor CPU startup.

use super::local::{self, Entry};
use core::ops::Range;

#[cfg(target_arch = "aarch64")]
mod aarch64;
#[cfg(target_arch = "aarch64")]
use aarch64 as native;
#[cfg(target_arch = "riscv64")]
pub(in crate::metal) mod riscv64;
#[cfg(target_arch = "riscv64")]
use riscv64 as native;
#[cfg(target_arch = "x86_64")]
mod x86_64;
#[cfg(target_arch = "x86_64")]
use x86_64 as native;

/// Observe the current CPU while it is using its assigned ordinary stack.
pub(in crate::metal) fn admit(stack: Range<usize>) -> Option<Entry> {
    if !stack.contains(&local::stack_pointer()) {
        return None;
    }
    native::verify()
}

/// Confirm the current CPU identity on machines with a native identity register.
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
pub(in crate::metal) fn matches_identity(processor: usize) -> bool {
    processor == native::identity()
}
