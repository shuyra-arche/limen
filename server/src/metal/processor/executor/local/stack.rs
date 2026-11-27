//! Aligned, image-owned storage used as a CPU stack.
use core::{cell::UnsafeCell, mem::MaybeUninit, ops::Range};

#[repr(C, align(16))]
pub struct Stack<const BYTES: usize>(UnsafeCell<[MaybeUninit<u8>; BYTES]>);

// A shared reference only describes the extent. Actual use as a machine
// stack requires exclusive native startup or terminal entry.
unsafe impl<const BYTES: usize> Sync for Stack<BYTES> {}

impl<const BYTES: usize> Stack<BYTES> {
    pub const fn new() -> Self {
        assert!(BYTES != 0 && BYTES.is_multiple_of(16));
        Self(UnsafeCell::new([MaybeUninit::uninit(); BYTES]))
    }

    pub fn range(&self) -> Range<usize> {
        self.extent().range()
    }

    pub(in crate::metal) fn top(&self) -> usize {
        self.extent().top()
    }

    pub(in crate::metal) const fn extent(&self) -> StackExtent {
        StackExtent {
            base: self.0.get().cast::<u8>(),
            bytes: BYTES,
        }
    }
}

impl<const BYTES: usize> Default for Stack<BYTES> {
    fn default() -> Self {
        Self::new()
    }
}

/// Private extent of an image-provided stack.
pub(in crate::metal) struct StackExtent {
    base: *mut u8,
    bytes: usize,
}

// The extent only reads addresses and is immutable in resident descriptors.
unsafe impl Sync for StackExtent {}

impl StackExtent {
    pub(in crate::metal) fn range(&self) -> Range<usize> {
        self.base.addr()..self.top()
    }

    pub(in crate::metal) fn top(&self) -> usize {
        self.base.wrapping_add(self.bytes).addr()
    }
}
