//! Image-owned storage for platforms entering through a loader continuation.
use core::ops::Range;

#[repr(align(16))]
pub(super) struct Stack([u8; 32768]);

pub(super) static mut STACK: Stack = Stack([0; 32768]);

/// Memory reserved by this image for the bootstrap CPU.
pub fn stack_range() -> Range<usize> {
    let start = unsafe { (&raw const STACK.0) as usize };
    start..start + size_of::<Stack>()
}
