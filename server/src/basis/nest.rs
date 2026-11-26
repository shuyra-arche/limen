//! Backing storage lent by the trusted platform to the Bare Kernel Nest.

use core::{cell::UnsafeCell, mem::MaybeUninit, ptr};

use crate::metal::execution::Context;

const STACK_BYTES: usize = 16 * 1024;
pub(crate) const STACK_CAPACITY: usize = 2;

#[repr(align(16))]
struct Stack(UnsafeCell<[MaybeUninit<u8>; STACK_BYTES]>);

pub(crate) struct Storage {
    stack: Stack,
    context: Context,
}

// Only the current stack binding can use these bytes. The caller arranges
// one active Hart and settles its leave before reuse.
unsafe impl Sync for Storage {}

impl Storage {
    const fn new() -> Self {
        Self {
            stack: Stack(UnsafeCell::new([MaybeUninit::uninit(); STACK_BYTES])),
            context: Context::new(),
        }
    }

    pub(crate) fn stack_top(&self) -> usize {
        self.stack
            .0
            .get()
            .cast::<u8>()
            .wrapping_add(STACK_BYTES)
            .addr()
    }

    pub(crate) fn context(&self) -> &Context {
        &self.context
    }
}

/// Storage for two trusted Kernel Units, each with a private 16 KiB stack.
/// The platform places this storage in the resident Kernel image.
pub struct Stacks([Storage; STACK_CAPACITY]);

impl Stacks {
    pub const fn new() -> Self {
        Self([const { Storage::new() }; STACK_CAPACITY])
    }
}

impl Default for Stacks {
    fn default() -> Self {
        Self::new()
    }
}

pub(crate) struct KernelNest {
    supply: &'static Stacks,
    available: [bool; STACK_CAPACITY],
}

/// Exclusive use of a backing slot until the Unit retires its continuation.
pub(crate) struct StackBinding {
    slot: usize,
    storage: &'static Storage,
}

impl StackBinding {
    pub(crate) fn storage(&self) -> &'static Storage {
        self.storage
    }
}

impl KernelNest {
    pub(crate) fn bare(stacks: &'static mut Stacks) -> Self {
        Self {
            supply: stacks,
            available: [true; STACK_CAPACITY],
        }
    }

    pub(crate) fn bind(&mut self) -> Option<StackBinding> {
        let slot = self.available.iter().position(|available| *available)?;
        self.available[slot] = false;
        Some(StackBinding {
            slot,
            storage: &self.supply.0[slot],
        })
    }

    /// Clear the saved continuation and return this slot to the Nest.
    ///
    /// The caller must have settled every occupancy and forbidden future entry.
    pub(crate) unsafe fn unbind(&mut self, binding: StackBinding) {
        assert!(ptr::eq(&self.supply.0[binding.slot], binding.storage));
        assert!(!self.available[binding.slot]);
        unsafe { binding.storage.context().retire() };
        self.available[binding.slot] = true;
    }
}
