//! Aligned, image-owned storage used as a CPU stack.
use core::{cell::UnsafeCell, mem::MaybeUninit};

#[repr(C, align(16))]
pub(crate) struct Stack<const SIZE: usize>(UnsafeCell<[MaybeUninit<u8>; SIZE]>);

impl<const SIZE: usize> Stack<SIZE> {
    pub(in crate::metal) const fn new() -> Self {
        Self(UnsafeCell::new([MaybeUninit::uninit(); SIZE]))
    }

    pub(in crate::metal) fn top(&self) -> usize {
        self.0.get().cast::<u8>().wrapping_add(SIZE).addr()
    }
}
