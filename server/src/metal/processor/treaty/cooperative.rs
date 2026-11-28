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
use crate::metal::processor::execution::cooperative::Stop;

pub(crate) fn cooperative_ready() -> bool {
    native::ready()
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
        unsafe { self.0.get().write(native::Saved::initial(stack, entry)) };
    }

    /// Requires an inactive context with no permitted future entry.
    pub(crate) unsafe fn retire(&self) {
        unsafe { self.0.get().write(native::Saved::EMPTY) };
    }
}

impl<Occupancy> LeaveReceipt<Occupancy> {
    pub(crate) fn occupancy(self) -> Occupancy {
        self.occupancy
    }

    #[cfg(target_arch = "riscv64")]
    pub(crate) fn discarded(self) -> bool {
        self.discarded
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
        assert!(matches!(self.record.state.get(), State::Idle));
        assert!(can_switch());
        self.record.state.set(State::Running(occupancy));
        unsafe { self.record.dispatcher.switch(context, argument) };
        // The dispatcher is executing on its original stack again. Only now
        // does it publish a Leave receipt for the original occupancy.
        self.record.state.set(State::Stopped(LeaveReceipt {
            occupancy,
            #[cfg(target_arch = "riscv64")]
            discarded: matches!(self.record.stop.replace(Stop::Closed), Stop::Returning),
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
        assert_eq!(stopped.discarded(), receipt.discarded());
        self.record.state.set(State::Idle);
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
        });
        true
    }

    pub(crate) fn request_stop(&self, occupancy: Occupancy, interrupt: TimerInterrupt<'_>) -> bool {
        let Stop::Allowed { start, end } = self.record.stop.get() else {
            return false;
        };
        if self.running() != Some(occupancy)
            || !interrupt.request_return(&(start..end), &self.record.dispatcher)
        {
            return false;
        }
        self.record.stop.set(Stop::Returning);
        true
    }
}
