//! Resident continuation storage supplied by the trusted platform.

use core::{cell::UnsafeCell, mem::MaybeUninit, ptr};

use crate::metal::execution::cooperative::Context;

const STACK_BYTES: usize = 16 * 1024;
pub(crate) const STACK_CAPACITY: usize = 2;

#[repr(align(16))]
struct Stack(UnsafeCell<[MaybeUninit<u8>; STACK_BYTES]>);

pub(super) struct Storage {
    stack: Stack,
    context: Context,
}

// Only the Unit holding the stack lease can use these bytes. Its actual
// Room occupancy and machine access must settle before reuse.
unsafe impl Sync for Storage {}

impl Storage {
    const fn new() -> Self {
        Self {
            stack: Stack(UnsafeCell::new([MaybeUninit::uninit(); STACK_BYTES])),
            context: Context::new(),
        }
    }

    fn stack_top(&self) -> usize {
        self.stack
            .0
            .get()
            .cast::<u8>()
            .wrapping_add(STACK_BYTES)
            .addr()
    }

    pub(in crate::basis) fn context(&self) -> &Context {
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

/// The Unit owner keeps its leased continuations independent of End records.
pub(super) struct StackPool {
    supply: &'static Stacks,
    available: [bool; STACK_CAPACITY],
}

pub(super) struct StackLease {
    index: usize,
    storage: &'static Storage,
}

impl StackLease {
    pub(super) fn context(&self) -> &'static Context {
        self.storage.context()
    }
}

impl StackPool {
    pub(super) fn new(supply: &'static mut Stacks) -> Self {
        Self {
            supply,
            available: [true; STACK_CAPACITY],
        }
    }

    pub(super) fn claim(&mut self, start: extern "C" fn(*const ()) -> !) -> Option<StackLease> {
        let index = self.available.iter().position(|free| *free)?;
        let storage = &self.supply.0[index];
        // An available slot has no other continuation owner or machine user.
        unsafe { storage.context().prepare(storage.stack_top(), start) };
        self.available[index] = false;
        Some(StackLease { index, storage })
    }

    /// The former Unit must have settled every use and forbidden future entry.
    pub(super) unsafe fn release(&mut self, lease: StackLease) {
        assert!(ptr::eq(&self.supply.0[lease.index], lease.storage));
        assert!(!self.available[lease.index]);
        unsafe { lease.context().retire() };
        self.available[lease.index] = true;
    }
}
