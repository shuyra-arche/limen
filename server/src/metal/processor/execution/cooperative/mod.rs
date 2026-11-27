//! Resident continuation shapes and private cooperative machine state.

use core::{
    cell::{Cell, UnsafeCell},
    marker::PhantomData,
    sync::atomic::AtomicBool,
};

use crate::metal::processor::executor::local::stack::Stack;

#[cfg(target_arch = "aarch64")]
pub(in crate::metal::processor) mod aarch64;
#[cfg(target_arch = "riscv64")]
pub(in crate::metal::processor) mod riscv64;
#[cfg(target_arch = "x86_64")]
pub(in crate::metal::processor) mod x86_64;

#[cfg(target_arch = "aarch64")]
pub(in crate::metal::processor) use aarch64 as native;
#[cfg(target_arch = "riscv64")]
pub(in crate::metal::processor) use riscv64 as native;
#[cfg(target_arch = "x86_64")]
pub(in crate::metal::processor) use x86_64 as native;

/// One continuation in the actual linked machine's native save shape.
/// Its consumer owns every access and keeps its stack alive until departure.
pub(crate) struct Context(pub(in crate::metal::processor) UnsafeCell<native::Saved>);

impl Context {
    pub(crate) const fn new() -> Self {
        Self(UnsafeCell::new(native::Saved::EMPTY))
    }

    pub(in crate::metal::processor) unsafe fn switch(&self, incoming: &Self, argument: *const ()) {
        unsafe { native::switch(self.0.get(), incoming.0.get(), argument) };
    }
}

#[derive(Clone, Copy)]
pub(crate) struct LeaveReceipt<Occupancy>(pub(in crate::metal::processor) Occupancy);

#[derive(Clone, Copy)]
pub(in crate::metal::processor) enum State<Occupancy> {
    Idle,
    Running(Occupancy),
    Stopped(LeaveReceipt<Occupancy>),
}

pub(in crate::metal::processor) const DISPATCH_BYTES: usize = 32 * 1024;

pub(crate) struct Dispatcher<Occupancy> {
    pub(in crate::metal::processor) reserved: AtomicBool,
    pub(in crate::metal::processor) stack: Stack<DISPATCH_BYTES>,
    pub(in crate::metal::processor) started: Cell<bool>,
    pub(in crate::metal::processor) dispatcher: Context,
    pub(in crate::metal::processor) state: Cell<State<Occupancy>>,
}

// Only the LocalExecution for the corresponding physical Hart accesses the
// dispatcher and state. A suspended Unit never uses its old LocalExecution again.
unsafe impl<Occupancy: Send> Sync for Dispatcher<Occupancy> {}

pub(crate) struct LocalExecution<'a, Occupancy> {
    pub(in crate::metal::processor) hart: &'a Dispatcher<Occupancy>,
    local: PhantomData<*mut ()>,
}

impl<Occupancy> Dispatcher<Occupancy> {
    pub(in crate::metal::processor) const fn new() -> Self {
        Self {
            reserved: AtomicBool::new(false),
            stack: Stack::new(),
            started: Cell::new(false),
            dispatcher: Context::new(),
            state: Cell::new(State::Idle),
        }
    }

    /// The caller has established that this is the current physical Hart.
    pub(in crate::metal::processor) unsafe fn local(&self) -> LocalExecution<'_, Occupancy> {
        LocalExecution {
            hart: self,
            local: PhantomData,
        }
    }
}
