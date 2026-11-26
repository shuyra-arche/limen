//! Common cooperative occupancy and actual machine handoff.

use core::{cell::Cell, marker::PhantomData};

use super::Context;
use crate::metal::hart;

#[derive(Clone, Copy)]
pub(crate) struct LeaveReceipt<Occupancy>(Occupancy);

impl<Occupancy> LeaveReceipt<Occupancy> {
    pub(crate) fn occupancy(self) -> Occupancy {
        self.0
    }
}

#[derive(Clone, Copy)]
enum State<Occupancy> {
    Idle,
    Running(Occupancy),
    Stopped(LeaveReceipt<Occupancy>),
}

pub(crate) struct Hart<Occupancy> {
    dispatcher: Context,
    state: Cell<State<Occupancy>>,
}

// Only the LocalHart for the corresponding physical Hart accesses the
// dispatcher and state. A suspended Unit never uses its old LocalHart again.
unsafe impl<Occupancy: Send> Sync for Hart<Occupancy> {}

pub(crate) struct LocalHart<'a, Occupancy> {
    hart: &'a Hart<Occupancy>,
    local: PhantomData<*mut ()>,
}

impl<Occupancy> Hart<Occupancy> {
    pub(crate) const fn new() -> Self {
        Self {
            dispatcher: Context::new(),
            state: Cell::new(State::Idle),
        }
    }

    /// The caller has established that this is the current physical Hart.
    pub(crate) unsafe fn local(&self) -> LocalHart<'_, Occupancy> {
        LocalHart {
            hart: self,
            local: PhantomData,
        }
    }
}

impl<Occupancy: Copy + Eq> LocalHart<'_, Occupancy> {
    pub(crate) fn running(&self) -> Option<Occupancy> {
        match self.hart.state.get() {
            State::Running(occupancy) => Some(occupancy),
            _ => None,
        }
    }

    /// The caller holds the exact execution occupancy. Its live kernel stack
    /// and continuation must persist until the caller settles the receipt.
    pub(crate) unsafe fn enter(
        &self,
        occupancy: Occupancy,
        context: &Context,
        argument: *const (),
    ) {
        assert!(matches!(self.hart.state.get(), State::Idle));
        assert!(hart::can_switch());
        self.hart.state.set(State::Running(occupancy));
        unsafe { self.hart.dispatcher.switch(context, argument) };
        // The dispatcher is executing on its original stack again. The old
        // instruction and stack are saved but cannot execute without a new
        // occupancy.
        self.hart.state.set(State::Stopped(LeaveReceipt(occupancy)));
    }

    /// Requires invocation on the stack of this exact running occupancy.
    pub(crate) unsafe fn leave(&self, occupancy: Occupancy, context: &Context) {
        assert!(self.running() == Some(occupancy));
        assert!(hart::can_switch());
        unsafe { context.switch(&self.hart.dispatcher, core::ptr::null()) };
    }

    pub(crate) fn stopped(&self) -> Option<LeaveReceipt<Occupancy>> {
        match self.hart.state.get() {
            State::Stopped(receipt) => Some(receipt),
            _ => None,
        }
    }

    pub(crate) fn settle(&self, receipt: LeaveReceipt<Occupancy>) {
        assert!(self.stopped().map(LeaveReceipt::occupancy) == Some(receipt.occupancy()));
        self.hart.state.set(State::Idle);
    }
}
