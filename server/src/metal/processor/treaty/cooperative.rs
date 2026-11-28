//! Cooperative construction, original continuations and departure receipts.

#[cfg(target_arch = "riscv64")]
use core::ops::Range;
use core::sync::atomic::Ordering::Relaxed;

use crate::metal::processor::{
    execution::cooperative::{DISPATCH_BYTES, State, native},
    executor::local,
};

pub(crate) use crate::metal::processor::execution::cooperative::{
    Context, Dispatcher, LeaveReceipt, LocalExecution,
};

#[cfg(target_arch = "riscv64")]
use super::TimerInterrupt;
use super::{ProcessorId, can_switch};
#[cfg(target_arch = "riscv64")]
use crate::metal::processor::execution::cooperative::{Departure, Stop};

pub(crate) fn cooperative_ready() -> bool {
    native::ready()
}

#[cfg(target_arch = "riscv64")]
pub(crate) fn budget_ready() -> bool {
    native::ready()
        && local::ordinary_local()
            .and_then(|executor| executor.runtime())
            .is_some_and(|runtime| runtime.covers_budget())
}

/// Discover the current Executor's bound machine record. The local
/// locator grants no Unit admission or Occupancy.
pub(crate) fn current<Occupancy: 'static>(
    records: &'static [Dispatcher<Occupancy>],
) -> Option<(ProcessorId, LocalExecution<'static, Occupancy>)> {
    let (id, slot) = local::cooperative_slot(records.as_ptr().addr())?;
    Some((id, unsafe { records.get(slot)?.local() }))
}

#[cfg(target_arch = "riscv64")]
pub(crate) fn current_in_interrupt<Occupancy: 'static>(
    records: &'static [Dispatcher<Occupancy>],
    interrupt: &TimerInterrupt<'_>,
) -> Option<LocalExecution<'static, Occupancy>> {
    let slot = local::interrupt_cooperative_slot(records.as_ptr().addr(), interrupt.processor())?;
    Some(unsafe { records.get(slot)?.local() })
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

pub(crate) enum RuntimeError {
    Bound,
    Capacity,
}

/// Bind one permanent machine record and transfer to its own stack.
///
/// The caller has verified ordinary entry and excluded local interruption.
/// The entry and argument must remain resident across the stack switch.
pub(crate) unsafe fn start_runtime<Occupancy: 'static>(
    records: &'static [Dispatcher<Occupancy>],
    entry: extern "C" fn(*const ()) -> !,
    argument: *const (),
) -> Result<!, RuntimeError> {
    if local::cooperative_bound() {
        return Err(RuntimeError::Bound);
    }
    let slot = records
        .iter()
        .position(|record| {
            record
                .reserved
                .compare_exchange(false, true, Relaxed, Relaxed)
                .is_ok()
        })
        .ok_or(RuntimeError::Capacity)?;
    unsafe { local::bind_cooperative(records.as_ptr().addr(), slot) };
    unsafe { native::start_stack(records[slot].stack.top(), entry, argument) }
}

impl Context {
    /// Requires exclusive use of an inactive context and a live stack extent.
    pub(crate) unsafe fn prepare(&self, stack: usize, entry: extern "C" fn(*const ()) -> !) {
        assert!(stack.is_multiple_of(16));
        unsafe { self.saved.get().write(native::Saved::initial(stack, entry)) };
    }

    /// Requires an inactive context with no permitted future entry.
    pub(crate) unsafe fn retire(&self) {
        unsafe { self.saved.get().write(native::Saved::EMPTY) };
        #[cfg(target_arch = "riscv64")]
        unsafe {
            *self.suspension.get() = None;
        }
    }
}

impl<Occupancy> LeaveReceipt<Occupancy> {
    pub(crate) fn occupancy(self) -> Occupancy {
        self.occupancy
    }

    #[cfg(target_arch = "riscv64")]
    pub(crate) fn discarded(self) -> bool {
        self.departure == Departure::Discarded
    }

    #[cfg(target_arch = "riscv64")]
    pub(crate) fn suspended(self) -> bool {
        self.departure == Departure::Suspended
    }

    #[cfg(target_arch = "riscv64")]
    pub(crate) fn unused(self) -> bool {
        self.departure == Departure::Unused
    }
}

impl<Occupancy: Copy + Eq> LocalExecution<'_, Occupancy> {
    pub(crate) fn on_dispatcher_stack(&self) -> bool {
        let top = self.record.stack.top();
        (top - DISPATCH_BYTES..top).contains(&local::stack_pointer())
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
        // A suspended IRQ frame requires a newly covered entry. A normal
        // cooperative dispatch may only consume its own ABI continuation.
        #[cfg(target_arch = "riscv64")]
        assert!(unsafe { (*context.suspension.get()).is_none() });
        self.transfer(occupancy, |dispatcher| unsafe {
            dispatcher.switch(context, argument)
        });
    }

    fn transfer(&self, occupancy: Occupancy, enter: impl FnOnce(&Context)) {
        assert!(matches!(self.record.state.get(), State::Idle));
        assert!(can_switch());
        self.record.state.set(State::Running(occupancy));
        enter(&self.record.dispatcher);
        // The dispatcher is executing on its original stack again. Only now
        // does it publish a Leave receipt for the original occupancy.
        self.record.state.set(State::Stopped(LeaveReceipt {
            occupancy,
            #[cfg(target_arch = "riscv64")]
            departure: match self.record.stop.replace(Stop::Closed) {
                Stop::Returning(departure) => departure,
                _ => Departure::Cooperative,
            },
        }));
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
        let stopped = self.stopped().expect("pending Leave receipt");
        assert!(stopped.occupancy() == receipt.occupancy());
        #[cfg(target_arch = "riscv64")]
        assert_eq!(stopped.departure, receipt.departure);
        self.record.state.set(State::Idle);
    }
}

#[cfg(target_arch = "riscv64")]
impl<Occupancy: Copy + Eq> LocalExecution<'_, Occupancy> {
    /// Close the one unused native start opportunity from the dispatcher.
    ///
    /// # Safety
    /// The ordinary caller owns the exact original Room reservation and has
    /// excluded every future start under it. It has never transferred to
    /// the Unit or started charging this occupancy.
    pub(crate) unsafe fn cancel_unstarted(&self, occupancy: Occupancy) {
        assert!(self.on_dispatcher_stack() && cooperative_ready() && can_switch());
        assert!(matches!(self.record.state.get(), State::Idle));
        assert!(matches!(self.record.stop.get(), Stop::Closed));
        self.record.state.set(State::Stopped(LeaveReceipt {
            occupancy,
            departure: Departure::Unused,
        }));
    }

    /// Enter this fixed-processor Unit with an admitted recoverable IRQ span.
    ///
    /// The caller owns the exact occupancy and Unit continuation and has
    /// confirmed stop coverage on the current processor before calling.
    /// All saved resources stay valid while the continuation is suspended.
    /// Every dynamic use of the span must be valid to suspend. The span
    /// and its stack remain resident until final departure is settled.
    pub(crate) unsafe fn enter_recoverable(
        &self,
        occupancy: Occupancy,
        context: &'static Context,
        argument: *const (),
        span: Range<usize>,
    ) {
        assert!(cooperative_ready() && can_switch());
        assert!(matches!(self.record.state.get(), State::Idle));
        assert!(matches!(self.record.stop.get(), Stop::Closed));
        assert!(!span.is_empty());
        let processor = local::ordinary_local()
            .expect("trusted local entry")
            .identity();
        let stored = unsafe { &mut *context.suspension.get() };
        assert!(
            stored
                .as_ref()
                .is_none_or(|saved| saved.valid_for(processor, &span))
        );
        let suspended = stored.take();
        self.record.stop.set(Stop::Allowed {
            start: span.start,
            end: span.end,
            retain: Some(context),
        });
        self.transfer(occupancy, |dispatcher| unsafe {
            match suspended {
                Some(saved) => saved.resume(dispatcher),
                None => dispatcher.switch(context, argument),
            }
        });
    }
}

#[cfg(target_arch = "riscv64")]
impl<Occupancy: Copy + Eq> LocalExecution<'static, Occupancy> {
    pub(crate) fn allow_stop_in(&self, span: Range<usize>) -> bool {
        if span.is_empty() || !matches!(self.record.stop.get(), Stop::Closed) {
            return false;
        }
        self.record.stop.set(Stop::Allowed {
            start: span.start,
            end: span.end,
            retain: None,
        });
        true
    }

    pub(crate) fn request_stop(&self, occupancy: Occupancy, interrupt: TimerInterrupt<'_>) -> bool {
        let Stop::Allowed { start, end, retain } = self.record.stop.get() else {
            return false;
        };
        if self.running() != Some(occupancy)
            || !interrupt.request_return(&(start..end), &self.record.dispatcher, retain)
        {
            return false;
        }
        let departure = if retain.is_some() {
            Departure::Suspended
        } else {
            Departure::Discarded
        };
        self.record.stop.set(Stop::Returning(departure));
        true
    }
}
