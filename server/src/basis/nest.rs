//! Backing storage lent by the trusted platform to the Bare Kernel Nest.

use core::{cell::UnsafeCell, mem::MaybeUninit};

use super::unit::CAPACITY;
use crate::metal::arch::execution::Context;

const STACK_BYTES: usize = 16 * 1024;

#[repr(align(16))]
struct Stack(UnsafeCell<[MaybeUninit<u8>; STACK_BYTES]>);

pub(crate) struct Storage {
    stack: Stack,
    context: Context,
}

// Stack and saved registers are accessed only for a reserved Unit slot.
// Room publishes exactly one active Hart per slot and releases it on leave.
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
pub struct Stacks([Storage; CAPACITY]);

impl Stacks {
    pub const fn new() -> Self {
        Self([const { Storage::new() }; CAPACITY])
    }
}

impl Default for Stacks {
    fn default() -> Self {
        Self::new()
    }
}

pub(crate) struct KernelNest(&'static Stacks);

impl KernelNest {
    pub(crate) fn bare(stacks: &'static mut Stacks) -> Self {
        Self(stacks)
    }

    // The Unit index owns allocation and retirement of these fixed slots.
    pub(crate) fn bind(&self, slot: usize) -> &'static Storage {
        &self.0.0[slot]
    }
}
