//! Resident continuation shapes and private cooperative machine state.

use core::{
    cell::{Cell, UnsafeCell},
    marker::PhantomData,
    sync::atomic::AtomicBool,
};

use crate::metal::processor::executor::local::stack::Stack;

#[cfg(target_arch = "riscv64")]
use super::runtime::Suspension;

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
pub(crate) struct Context {
    pub(in crate::metal::processor) saved: UnsafeCell<native::Saved>,
    #[cfg(target_arch = "riscv64")]
    pub(in crate::metal::processor) suspension: UnsafeCell<Option<Suspension>>,
}

impl Context {
    pub(crate) const fn new() -> Self {
        Self {
            saved: UnsafeCell::new(native::Saved::EMPTY),
            #[cfg(target_arch = "riscv64")]
            suspension: UnsafeCell::new(None),
        }
    }

    pub(in crate::metal::processor) unsafe fn switch(&self, incoming: &Self, argument: *const ()) {
        unsafe { native::switch(self.saved.get(), incoming.saved.get(), argument) };
    }
}

#[derive(Clone, Copy)]
pub(crate) struct LeaveReceipt<Occupancy> {
    pub(in crate::metal::processor) occupancy: Occupancy,
    #[cfg(target_arch = "riscv64")]
    pub(in crate::metal::processor) departure: Departure,
}

#[cfg(target_arch = "riscv64")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::metal::processor) enum Departure {
    Cooperative,
    Discarded,
    Suspended,
    Unused,
}

#[derive(Clone, Copy)]
pub(in crate::metal::processor) enum State<Occupancy> {
    Idle,
    Running(Occupancy),
    Stopped(LeaveReceipt<Occupancy>),
}

pub(in crate::metal::processor) const DISPATCH_BYTES: usize = 32 * 1024;

#[cfg(target_arch = "riscv64")]
#[derive(Clone, Copy)]
pub(in crate::metal::processor) enum Stop {
    Closed,
    Allowed {
        start: usize,
        end: usize,
        retain: Option<&'static Context>,
    },
    Returning(Departure),
}

pub(crate) struct Dispatcher<Occupancy> {
    pub(in crate::metal::processor) reserved: AtomicBool,
    pub(in crate::metal::processor) stack: Stack<DISPATCH_BYTES>,
    pub(in crate::metal::processor) dispatcher: Context,
    pub(in crate::metal::processor) state: Cell<State<Occupancy>>,
    #[cfg(target_arch = "riscv64")]
    pub(in crate::metal::processor) stop: Cell<Stop>,
}

// Only the LocalExecution for the matching Executor accesses the
// dispatcher and state. A suspended Unit never uses its old LocalExecution again.
unsafe impl<Occupancy: Send> Sync for Dispatcher<Occupancy> {}

pub(crate) struct LocalExecution<'a, Occupancy> {
    pub(in crate::metal::processor) record: &'a Dispatcher<Occupancy>,
    local: PhantomData<*mut ()>,
}

impl<Occupancy> Dispatcher<Occupancy> {
    pub(in crate::metal::processor) const fn new() -> Self {
        Self {
            reserved: AtomicBool::new(false),
            stack: Stack::new(),
            dispatcher: Context::new(),
            state: Cell::new(State::Idle),
            #[cfg(target_arch = "riscv64")]
            stop: Cell::new(Stop::Closed),
        }
    }

    /// The caller has established that this record belongs to the current Executor.
    pub(in crate::metal::processor) unsafe fn local(&self) -> LocalExecution<'_, Occupancy> {
        LocalExecution {
            record: self,
            local: PhantomData,
        }
    }
}
