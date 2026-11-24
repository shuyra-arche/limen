//! Image-owned, exclusively reserved storage for the primary boot CPU.
use core::ops::Range;

use crate::metal::boot::{STACK, STACK_BYTES, stack_pointer};

#[repr(C, align(16))]
pub(in crate::metal) struct Stack([u8; STACK_BYTES]);

impl Stack {
    pub(in crate::metal) const fn new() -> Self {
        Self([0; STACK_BYTES])
    }
}

/// Ordinary stack reservation of the primary boot CPU.
pub fn stack_range() -> Range<usize> {
    let start = unsafe { (&raw const STACK.0).addr() };
    start..start + size_of::<Stack>()
}

pub(crate) fn active() -> bool {
    stack_range().contains(&stack_pointer())
}
