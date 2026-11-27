//! Cooperative construction, original continuations and departure receipts.

use core::sync::atomic::Ordering::Relaxed;

use crate::metal::processor::{
    execution::cooperative::{DISPATCH_BYTES, State, native},
    executor::local,
};

pub(crate) use crate::metal::processor::execution::cooperative::{
    Context, Dispatcher, LeaveReceipt, LocalExecution,
};

use super::{ProcessorId, can_switch};

pub(crate) fn cooperative_ready() -> bool {
    native::ready()
}

/// Whether the current Executor has already joined the cooperative prefix.
pub(crate) fn attached() -> bool {
    local::cooperative_attached()
}

/// Discover the current Executor's bound machine record. The local
/// locator grants no Unit admission or Occupancy.
pub(crate) fn current<Occupancy: 'static>(
    records: &'static [Dispatcher<Occupancy>],
) -> Option<(ProcessorId, LocalExecution<'static, Occupancy>)> {
    let (id, slot) = local::cooperative_slot(records.as_ptr().addr())?;
    Some((id, unsafe { records.get(slot)?.local() }))
}

/// Resident storage for the machine records, supplied by the trusted image.
pub struct Dispatchers<Occupancy, const CAPACITY: usize> {
    records: [Dispatcher<Occupancy>; CAPACITY],
}

impl<Occupancy, const CAPACITY: usize> Dispatchers<Occupancy, CAPACITY> {
    pub const fn new() -> Self {
        Self {
            records: [const { Dispatcher::new() }; CAPACITY],
        }
    }

    pub(crate) fn records(&'static mut self) -> &'static [Dispatcher<Occupancy>] {
        &self.records
    }
}

impl<Occupancy, const CAPACITY: usize> Default for Dispatchers<Occupancy, CAPACITY> {
    fn default() -> Self {
        Self::new()
    }
}

/// One unused machine record held while the matching Room admission can fail.
pub(crate) struct Reservation<Occupancy: 'static> {
    records: &'static [Dispatcher<Occupancy>],
    slot: Option<usize>,
}

pub(crate) fn reserve<Occupancy: 'static>(
    records: &'static [Dispatcher<Occupancy>],
) -> Option<Reservation<Occupancy>> {
    let slot = records.iter().position(|record| {
        record
            .reserved
            .compare_exchange(false, true, Relaxed, Relaxed)
            .is_ok()
    })?;
    Some(Reservation {
        records,
        slot: Some(slot),
    })
}

impl<Occupancy> Reservation<Occupancy> {
    /// Room has admitted the current Executor and excludes competing ordinary entry
    /// until its original machine record has been published.
    pub(crate) unsafe fn bind(mut self) {
        let slot = self.slot.expect("unbound machine record");
        unsafe { local::bind_cooperative(self.records.as_ptr().addr(), slot) };
        self.slot = None;
    }
}

impl<Occupancy> Drop for Reservation<Occupancy> {
    fn drop(&mut self) {
        if let Some(slot) = self.slot {
            self.records[slot].reserved.store(false, Relaxed);
        }
    }
}

impl Context {
    /// Requires exclusive use of an inactive context and a live stack extent.
    pub(crate) unsafe fn prepare(&self, stack: usize, entry: extern "C" fn(*const ()) -> !) {
        assert!(stack.is_multiple_of(16));
        unsafe { self.0.get().write(native::Saved::initial(stack, entry)) };
    }

    /// Requires an inactive context with no permitted future entry.
    pub(crate) unsafe fn retire(&self) {
        unsafe { self.0.get().write(native::Saved::EMPTY) };
    }
}

impl<Occupancy> LeaveReceipt<Occupancy> {
    pub(crate) fn occupancy(self) -> Occupancy {
        self.0
    }
}

impl<Occupancy: Copy + Eq> LocalExecution<'_, Occupancy> {
    pub(crate) fn started(&self) -> bool {
        self.record.started.get()
    }

    pub(crate) fn on_dispatcher_stack(&self) -> bool {
        let top = self.record.stack.top();
        self.started() && (top - DISPATCH_BYTES..top).contains(&local::stack_pointer())
    }

    /// The caller has excluded every previous entry and checked this processor's
    /// ordinary profile. The previous frame stays resident with the borrowed
    /// dispatch input; this path never makes that stack reclaimable.
    pub(crate) unsafe fn start<F: FnMut() -> !>(&self, dispatch: &mut F) -> ! {
        extern "C" fn enter<F: FnMut() -> !>(argument: *const ()) -> ! {
            let dispatch = unsafe { &mut *argument.cast_mut().cast::<F>() };
            dispatch()
        }

        assert!(!self.record.started.replace(true));
        unsafe {
            native::start_stack(
                self.record.stack.top(),
                enter::<F>,
                core::ptr::from_mut(dispatch).cast(),
            )
        }
    }

    pub(crate) fn running(&self) -> Option<Occupancy> {
        match self.record.state.get() {
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
        assert!(matches!(self.record.state.get(), State::Idle));
        assert!(can_switch());
        self.record.state.set(State::Running(occupancy));
        unsafe { self.record.dispatcher.switch(context, argument) };
        // The dispatcher is executing on its original stack again. The old
        // instruction and stack are saved but cannot execute without a new
        // occupancy.
        self.record
            .state
            .set(State::Stopped(LeaveReceipt(occupancy)));
    }

    /// Requires invocation on the stack of this exact running occupancy.
    pub(crate) unsafe fn leave(&self, occupancy: Occupancy, context: &Context) {
        assert!(self.running() == Some(occupancy));
        assert!(can_switch());
        unsafe { context.switch(&self.record.dispatcher, core::ptr::null()) };
    }

    pub(crate) fn stopped(&self) -> Option<LeaveReceipt<Occupancy>> {
        match self.record.state.get() {
            State::Stopped(receipt) => Some(receipt),
            _ => None,
        }
    }

    pub(crate) fn settle(&self, receipt: LeaveReceipt<Occupancy>) {
        assert!(self.stopped().map(LeaveReceipt::occupancy) == Some(receipt.occupancy()));
        self.record.state.set(State::Idle);
    }
}
