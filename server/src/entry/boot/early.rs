//! Single-CPU diagnostic entry. The loader retains all resource reservations.
use core::ops::Range;

#[cfg(target_arch = "aarch64")]
pub use crate::metal::aarch64::enter;
#[cfg(target_arch = "x86_64")]
pub use crate::metal::x86_64::enter;

#[repr(align(16))]
pub(crate) struct Stack(pub(crate) [u8; 32768]);

pub(crate) static mut STACK: Stack = Stack([0; 32768]);

/// Memory reserved by this image for the bootstrap CPU.
pub fn stack_range() -> Range<usize> {
    let start = unsafe { (&raw const STACK.0) as usize };
    start..start + size_of::<Stack>()
}
