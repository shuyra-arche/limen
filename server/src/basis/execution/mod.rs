//! Coordinates the existing cooperative Unit, Room and machine handoff.

#[cfg(target_arch = "riscv64")]
use crate::metal::processor::treaty::TimerInterrupt;
#[cfg(target_arch = "riscv64")]
use core::ops::Range;
use core::{marker::PhantomData, num::NonZeroUsize};

#[cfg(target_arch = "riscv64")]
mod fund;
#[cfg(target_arch = "riscv64")]
pub use fund::{
    ChargeSettlement, Fund, FundConfig, FundError, FundEstablishFailure, PeriodKey, PeriodRecord,
    PeriodSettlement,
};

use crate::{
    basis::{
        room::{
            self, AdmissionId, IdleObservation, LeaveDestination, Occupancy, Participants, Queue,
            Room,
        },
        sign::{Deadlines, Sign},
        unit::{self, End, Lifecycle, STACK_CAPACITY, Stacks, UnitId, UnitRecord, Units},
    },
    metal::{
        chrono::treaty::Deadline,
        interrupt::treaty::AbsoluteTimer,
        processor::treaty::{
            self, Dispatcher, Dispatchers, LeaveReceipt, LocalExecution, Major, ProcessorId,
            Terminal,
        },
        sync::treaty::{TurnAcquireError, TurnLock, TurnMode, TurnWorkspace, turn_capacity},
    },
};

type Entry = fn(&mut Current) -> u64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Busy,
    Capacity,
    IdentityExhausted,
    Gone,
    UnitNotLive,
    UnitNotEnding,
    UnitNotEnded,
    Protection,
    Timing,
    State,
    Unsupported,
    #[cfg(target_arch = "riscv64")]
    Budget,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeadlineProgress {
    Idle,
    Armed,
    /// The original receiver must accept the expired comparator's interrupt.
    AwaitInterrupt,
    Recheck,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeadlineError<T> {
    Execution(Error),
    Timer(T),
}

impl From<unit::Error> for Error {
    fn from(error: unit::Error) -> Self {
        match error {
            unit::Error::Capacity => Self::Capacity,
            unit::Error::IdentityExhausted => Self::IdentityExhausted,
            unit::Error::Gone => Self::Gone,
            unit::Error::NotLive => Self::UnitNotLive,
            unit::Error::NotEnding => Self::UnitNotEnding,
            unit::Error::NotEnded => Self::UnitNotEnded,
        }
    }
}

impl From<room::Error> for Error {
    fn from(error: room::Error) -> Self {
        match error {
            room::Error::Busy => Self::Busy,
            room::Error::Capacity => Self::Capacity,
            room::Error::Gone => Self::Gone,
            room::Error::State => Self::State,
            room::Error::Unsupported => Self::Unsupported,
            #[cfg(target_arch = "riscv64")]
            room::Error::Budget => Self::Budget,
            #[cfg(target_arch = "riscv64")]
            room::Error::Timing => Self::Timing,
        }
    }
}

impl From<TurnAcquireError> for Error {
    fn from(error: TurnAcquireError) -> Self {
        match error {
            TurnAcquireError::Busy => Self::Busy,
            TurnAcquireError::Capacity => Self::Capacity,
            TurnAcquireError::Context | TurnAcquireError::Empty => Self::Protection,
        }
    }
}

impl From<treaty::RuntimeError> for Error {
    fn from(error: treaty::RuntimeError) -> Self {
        match error {
            treaty::RuntimeError::Bound => Self::Busy,
            treaty::RuntimeError::Capacity => Self::Capacity,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EstablishError {
    WrongProcessor,
    Unsupported,
    Storage,
}

/// The original handoff and independent supplies, returned before ownership.
pub struct EstablishFailure<const PARTICIPANTS: usize> {
    pub major: Major,
    pub terminal: Terminal,
    pub stacks: &'static mut Stacks,
    pub records: &'static mut [UnitRecord<KernelBinding>],
    pub participants: &'static mut Participants<PARTICIPANTS>,
    pub dispatchers: &'static mut Dispatchers<Occupancy, PARTICIPANTS>,
    pub reason: EstablishError,
}

/// The trusted cooperative Unit entry and its actual placement constraint.
pub struct KernelBinding {
    entry: Entry,
    affinity: Option<ProcessorId>,
    queue: Queue,
    #[cfg(target_arch = "riscv64")]
    span: Option<(usize, usize)>,
}

/// The trusted runtime entry and its input in permanent storage.
pub struct RuntimeEntry<T: 'static> {
    entry: fn(&'static T) -> !,
    input: T,
}

impl<T: 'static> RuntimeEntry<T> {
    pub const fn new(entry: fn(&'static T) -> !, input: T) -> Self {
        Self { entry, input }
    }

    extern "C" fn enter(argument: *const ()) -> ! {
        // The only caller transfers a static reference to this original entry.
        let runtime = unsafe { &*argument.cast::<Self>() };
        (runtime.entry)(&runtime.input)
    }
}

/// Cooperative coordination of Unit and Room across participating Executors.
pub struct Execution {
    terminal: Terminal,
    units: TurnLock<Units<KernelBinding>>,
    room: TurnLock<Room<STACK_CAPACITY>>,
    dispatchers: &'static [Dispatcher<Occupancy>],
}

impl Execution {
    /// Consume the original native major relation. Each Executor participates
    /// after its own Attach. The supplies remain independent and resident.
    pub fn new<const PARTICIPANTS: usize>(
        major: Major,
        terminal: Terminal,
        stacks: &'static mut Stacks,
        records: &'static mut [UnitRecord<KernelBinding>],
        participants: &'static mut Participants<PARTICIPANTS>,
        dispatchers: &'static mut Dispatchers<Occupancy, PARTICIPANTS>,
    ) -> Result<Self, EstablishFailure<PARTICIPANTS>> {
        let reason = if !major.is_current() {
            Some(EstablishError::WrongProcessor)
        } else if !treaty::cooperative_ready() {
            Some(EstablishError::Unsupported)
        } else if !Units::accepts(records) || !participants.accepts() {
            Some(EstablishError::Storage)
        } else {
            None
        };
        if let Some(reason) = reason {
            return Err(EstablishFailure {
                major,
                terminal,
                stacks,
                records,
                participants,
                dispatchers,
                reason,
            });
        }
        Ok(Self {
            terminal,
            units: TurnLock::new(Units::new(stacks, records)),
            room: TurnLock::new(participants.room()),
            dispatchers: dispatchers.records(),
        })
    }

    fn with_unit_room<R>(
        &self,
        operation: impl FnOnce(&mut Units<KernelBinding>, &mut Room<STACK_CAPACITY>) -> Result<R, Error>,
    ) -> Result<R, Error> {
        let mut workspace = TurnWorkspace::<2>::new();
        let mut plan = workspace.plan();
        plan.insert(&self.units).expect("Unit owner");
        plan.insert(&self.room).expect("Room owner");
        // Returning from a Unit must be able to commit its terminal leave
        // under contention. Queue in canonical order before changing either owner.
        plan.acquire(TurnMode::Slow, |held| {
            let (units, room) = held
                .access_pair(&self.units, &self.room)
                .expect("complete Unit and Room plan");
            operation(units, room)
        })?
    }

    fn local(&self) -> Result<(ProcessorId, LocalExecution<'static, Occupancy>), Error> {
        treaty::try_current_processor().ok_or(Error::Protection)?;
        treaty::current(self.dispatchers).ok_or(Error::Unsupported)
    }

    /// Attach the current Executor from its permanent dispatcher stack.
    pub fn attach(&self) -> Result<(), Error> {
        if !treaty::cooperative_ready() {
            return Err(Error::Unsupported);
        }
        let (id, local) = self.local()?;
        if !treaty::can_switch() {
            return Err(Error::Protection);
        }
        if !local.on_dispatcher_stack() {
            return Err(Error::Protection);
        }
        if turn_capacity().ok_or(Error::Protection)? < 2 {
            return Err(Error::Capacity);
        }
        self.room.acquire(|room| room.attach(id))??;
        Ok(())
    }

    /// Construct a dormant Kernel Unit. Its entry's return value is its End.
    ///
    /// # Safety
    ///
    /// The caller supplies a trusted supervisor entry whose compiler and
    /// callees fit the 16 KiB stack, preserve the established translation and trap
    /// configuration, processor-local registers, and the selected backend's
    /// interrupt and extension restrictions. Any local borrow must allow suspension at
    /// `yield_now`. The Unit stays on the constructing processor.
    pub unsafe fn create(&self, entry: Entry) -> Result<UnitId, Error> {
        let processor = treaty::try_current_processor().ok_or(Error::Protection)?;
        self.create_unit(entry, Some(processor))
    }

    /// Construct a Unit eligible to resume on any attached compatible processor.
    ///
    /// # Safety
    ///
    /// The entry must satisfy `create` except for affinity. Every live value
    /// at Yield must allow transfer to another processor. The entry must release
    /// processor-local borrows and protection before Yield and reacquire local state
    /// on the destination processor.
    pub unsafe fn create_migratable(&self, entry: Entry) -> Result<UnitId, Error> {
        self.create_unit(entry, None)
    }

    fn create_unit(&self, entry: Entry, affinity: Option<ProcessorId>) -> Result<UnitId, Error> {
        self.publish_unit(KernelBinding {
            entry,
            affinity,
            queue: Queue::Ordinary,
            #[cfg(target_arch = "riscv64")]
            span: None,
        })
    }

    fn publish_unit(&self, binding: KernelBinding) -> Result<UnitId, Error> {
        self.units.acquire(|units| {
            let unit = units.vacant()?;
            units.publish(unit, binding, start)?;
            Ok(unit)
        })?
    }

    pub fn submit(&self, unit: UnitId) -> Result<AdmissionId, Error> {
        self.with_unit_room(|units, room| {
            let binding = units.live(unit)?.0;
            room.submit(unit, binding.affinity, binding.queue)
                .map_err(Error::from)
        })
    }

    /// Select cancellation for a Unit that has never been admitted to Room.
    /// A successful submission wins against this operation. After a
    /// cancellation, use `finish_end` and `reap` to release the slot.
    pub fn stop_dormant(&self, unit: UnitId) -> Result<(), Error> {
        self.with_unit_room(|units, room| {
            if room.admitted(unit) {
                return Err(Error::Busy);
            }
            units.begin_end(unit, End::Cancelled).map_err(Error::from)
        })
    }

    /// Transfer once to the current Executor's resident 32 KiB dispatcher stack.
    /// The entry can then attach this Executor to Room. Success never returns.
    ///
    /// # Safety
    /// The trusted dispatch loop and its callees must fit the supplied stack
    /// and preserve the native profile and local entry. Before switching,
    /// settle or transfer all obligations of the abandoned frames. The owner
    /// of the boot material must establish its independent retirement evidence.
    pub unsafe fn run<T>(&'static self, entry: &'static RuntimeEntry<T>) -> Result<!, Error> {
        treaty::try_current_processor().ok_or(Error::Protection)?;
        if !treaty::cooperative_ready() {
            return Err(Error::Unsupported);
        }
        if !treaty::can_switch() {
            return Err(Error::Protection);
        }
        unsafe {
            treaty::start_runtime(
                self.dispatchers,
                RuntimeEntry::<T>::enter,
                core::ptr::from_ref(entry).cast(),
            )
        }
        .map_err(Error::from)
    }

    fn dispatcher(&self) -> Result<(ProcessorId, LocalExecution<'_, Occupancy>), Error> {
        let (id, local) = self.local()?;
        if !treaty::cooperative_ready() {
            return Err(Error::Unsupported);
        }
        if !treaty::can_switch() {
            return Err(Error::Protection);
        }
        if !local.on_dispatcher_stack() {
            return Err(if local.running().is_some() {
                Error::Busy
            } else {
                Error::Protection
            });
        }
        Ok((id, local))
    }

    /// Observe an attached dispatcher's empty compatible queue in ordinary execution.
    /// The later hardware wait can check the observation without retaining a Turn.
    /// The caller supplies valid wake coverage for subsequent Ready publications.
    pub fn observe_idle(&self) -> Result<Option<IdleObservation>, Error> {
        let (processor, _) = self.dispatcher()?;
        self.room
            .acquire(|room| room.observe_idle(processor))?
            .map_err(Error::from)
    }

    /// Progress local pure-deadline waits and adjust their dedicated comparator.
    ///
    /// The dispatcher owns this timer for Sign and must arrange a native wake
    /// path while armed. Retry Recheck and retained timer effects before
    /// sleeping. AwaitInterrupt leaves the original comparator asserted
    /// for its native receiver, which must accept and finish that claim
    /// before the next comparison can be committed.
    ///
    /// # Safety
    /// The supplied comparator is dedicated to this Sign, is in the same
    /// local clock domain, and has no stop or wake obligations outside it.
    /// Reuse its original receiver through any unsettled native effects.
    /// Keep its interrupt handler from borrowing the comparator during this
    /// ordinary exclusive access.
    pub unsafe fn service_deadlines<T: AbsoluteTimer, const N: usize>(
        &self,
        sign: &Sign<'_, N>,
        timer: &mut T,
        budget: NonZeroUsize,
    ) -> Result<DeadlineProgress, DeadlineError<T::Failure>> {
        let (processor, _) = self.dispatcher().map_err(DeadlineError::Execution)?;
        if processor.get() != sign.clock.processor() {
            return Err(DeadlineError::Execution(Error::Timing));
        }

        let (next, due) = self
            .settle_deadlines(sign, budget.get().min(N))
            .map_err(DeadlineError::Execution)?;
        Self::program_deadlines(sign, timer, next, due)
    }

    fn program_deadlines<T: AbsoluteTimer, const N: usize>(
        sign: &Sign<'_, N>,
        timer: &mut T,
        next: Option<u64>,
        due: bool,
    ) -> Result<DeadlineProgress, DeadlineError<T::Failure>> {
        let timer_error = DeadlineError::Timer;
        if timer.pending_effect().map_err(timer_error)?.is_some() {
            timer.confirm_effect().map_err(timer_error)?;
            return Ok(DeadlineProgress::Recheck);
        }
        let armed = timer
            .confirmed_deadline()
            .map_err(timer_error)?
            .map(|deadline| sign.clock.count_for(deadline).ok_or(Error::Timing))
            .transpose()
            .map_err(DeadlineError::Execution)?;
        let now = sign
            .clock
            .sample()
            .ok_or(DeadlineError::Execution(Error::Timing))?
            .count();
        if armed.is_some_and(|count| count <= now) {
            // Preserve the source until its receiver acquires the original
            // IRQ. Deasserting a level source earlier can lose its claim.
            return Ok(DeadlineProgress::AwaitInterrupt);
        }
        if due || next.is_some_and(|count| count <= now) {
            return Ok(DeadlineProgress::Recheck);
        }

        let (result, count) = match (armed, next) {
            (None, Some(count)) => (timer.arm_at(sign.clock.deadline(count)), count),
            (Some(old), Some(count)) if count < old => {
                (timer.advance_deadline(sign.clock.deadline(count)), count)
            }
            (None, None) => return Ok(DeadlineProgress::Idle),
            _ => return Ok(DeadlineProgress::Armed),
        };
        if let Err(error) = result {
            // The backend samples again just before programming. If the
            // deadline was crossed and no effect escaped, ordinary expiry
            // can take over without retrying the obsolete write.
            let reached = sign
                .clock
                .sample()
                .is_some_and(|sample| sample.count() >= count);
            if reached && matches!(timer.pending_effect(), Ok(None)) {
                return Ok(DeadlineProgress::Recheck);
            }
            return Err(timer_error(error));
        }
        Ok(DeadlineProgress::Armed)
    }

    fn settle_deadlines<const N: usize>(
        &self,
        sign: &Sign<'_, N>,
        budget: usize,
    ) -> Result<(Option<u64>, bool), Error> {
        self.with_room_sign(sign, |room, deadlines| {
            let lower = sign.clock.sample().ok_or(Error::Timing)?.count();
            deadlines.discover_due(lower, budget);
            for _ in 0..budget {
                let Some((tock, occupancy, _)) = deadlines.due() else {
                    break;
                };
                match room.settle_timeout(occupancy, tock) {
                    Ok(()) => deadlines
                        .retire_due(tock, occupancy)
                        .expect("observed exact Due"),
                    Err(room::Error::Busy) => deadlines
                        .rotate_due(tock, occupancy)
                        .expect("observed exact Due"),
                    Err(error) => return Err(error.into()),
                }
            }
            Ok((deadlines.earliest(), deadlines.due().is_some()))
        })
    }

    fn with_room_sign<const N: usize, R>(
        &self,
        sign: &Sign<'_, N>,
        operation: impl FnOnce(&mut Room<STACK_CAPACITY>, &mut Deadlines<N>) -> Result<R, Error>,
    ) -> Result<R, Error> {
        let mut workspace = TurnWorkspace::<2>::new();
        let mut plan = workspace.plan();
        plan.insert(&self.room).expect("Room owner");
        plan.insert(&sign.deadlines).expect("Sign owner");
        plan.acquire(TurnMode::Slow, |held| {
            let (room, deadlines) = held
                .access_pair(&self.room, &sign.deadlines)
                .expect("complete Room and Sign plan");
            operation(room, deadlines)
        })?
    }

    fn wait_until<const N: usize>(
        &self,
        unit: UnitId,
        sign: &Sign<'_, N>,
        deadline: Deadline<'_>,
    ) -> Result<(), Error> {
        let (processor, local) = self.local()?;
        if !treaty::cooperative_ready() || !treaty::can_switch() {
            return Err(Error::Protection);
        }
        if processor.get() != sign.clock.processor() {
            return Err(Error::Timing);
        }
        let count = sign.clock.count_for(deadline).ok_or(Error::Timing)?;
        if count == u64::MAX {
            return Err(Error::Timing);
        }
        let occupancy = local
            .running()
            .filter(|active| active.unit() == unit)
            .ok_or(Error::Gone)?;
        let mut workspace = TurnWorkspace::<3>::new();
        let mut plan = workspace.plan();
        plan.insert(&self.units).expect("Unit owner");
        plan.insert(&self.room).expect("Room owner");
        plan.insert(&sign.deadlines).expect("Sign owner");
        let context = plan.acquire(TurnMode::Slow, |held| {
            let units = held.access(&self.units).expect("Unit protection");
            let (binding, context) = units.live(unit)?;
            if binding.affinity != Some(processor) {
                return Err(Error::Unsupported);
            }
            let (room, deadlines) = held
                .access_pair(&self.room, &sign.deadlines)
                .expect("Room and Sign protection");
            room.check_occupied(occupancy)?;
            if sign.clock.sample().ok_or(Error::Timing)?.count() >= count {
                return Ok(None);
            }
            let tock = deadlines
                .arm(occupancy, count)
                .map_err(|_| Error::Capacity)?;
            if let Err(error) = room.request_leave(occupancy, LeaveDestination::Wait(tock)) {
                deadlines.unarm(tock, occupancy).expect("just armed Tock");
                return Err(error.into());
            }
            Ok(Some(context))
        })??;
        if let Some(context) = context {
            // Resume only after the original receipt and timeout have settled.
            unsafe { local.leave(occupancy, context) };
        }
        Ok(())
    }

    /// Run one ready Unit until it yields or returns on its own stack.
    /// The original admission remains occupied until `vacate`.
    pub fn enter(&'static self) -> Result<Option<Occupancy>, Error> {
        let (id, local) = self.dispatcher()?;
        let Some((occupancy, context, entry)) = self.with_unit_room(|units, room| {
            let Some(unit) = room.candidate(id)? else {
                return Ok(None);
            };
            let (binding, context) = units.live(unit)?;
            let entry = binding.entry;
            Ok(Some((room.occupy(id, unit)?, context, entry)))
        })?
        else {
            return Ok(None);
        };
        let start = Start {
            execution: self,
            entry,
            #[cfg(target_arch = "riscv64")]
            fund: None,
        };
        // Room retains this exact occupancy until this processor completes the
        // leave. The initial continuation copies Start before it can suspend.
        unsafe { local.enter(occupancy, context, (&raw const start).cast()) };
        Ok(Some(occupancy))
    }

    pub fn pending_leave(&self) -> Result<Option<Occupancy>, Error> {
        Ok(self.local()?.1.stopped().map(LeaveReceipt::occupancy))
    }

    /// Select the dispatcher as the return target of the accepted local timer.
    /// The IRQ handler retains any unfinished native acknowledgement. A stale
    /// occupancy, unaccepted interrupt or instruction outside the allowed span
    /// leaves the original Unit frame unchanged. Only the resumed dispatcher
    /// can publish the corresponding Leave receipt.
    #[cfg(target_arch = "riscv64")]
    pub fn request_stop(
        &self,
        interrupt: TimerInterrupt<'_>,
        occupancy: Occupancy,
    ) -> Result<(), Error> {
        let local =
            treaty::current_in_interrupt(self.dispatchers, &interrupt).ok_or(Error::Protection)?;
        if local.running() != Some(occupancy) {
            return Err(Error::Gone);
        }
        local
            .request_stop(occupancy, interrupt)
            .then_some(())
            .ok_or(Error::Unsupported)
    }

    pub fn vacate(&self, occupancy: Occupancy) -> Result<(), Error> {
        let (_, local) = self.local()?;
        #[cfg(target_arch = "riscv64")]
        if let Some(receipt) = local
            .stopped()
            .filter(|receipt| receipt.occupancy() == occupancy && receipt.discarded())
        {
            return self.with_unit_room(|units, room| {
                room.check_unfunded(occupancy)?;
                units.live(occupancy.unit())?;
                // The terminal machine return has already left the discarded
                // stack. In the same Turn, close the Unit and its original
                // admission so there is no later entry to that continuation.
                room.request_leave(occupancy, LeaveDestination::Release)?;
                units
                    .begin_end(occupancy.unit(), End::Cancelled)
                    .expect("original live Unit");
                room.vacate(receipt).expect("original terminal departure");
                local.settle(receipt);
                Ok(())
            });
        }
        self.room.acquire(|room| {
            #[cfg(target_arch = "riscv64")]
            room.check_unfunded(occupancy)?;
            #[cfg(not(target_arch = "riscv64"))]
            room.check_occupied(occupancy)?;
            let receipt = local
                .stopped()
                .filter(|receipt| receipt.occupancy() == occupancy)
                .ok_or(Error::Busy)?;
            room.vacate(receipt)?;
            local.settle(receipt);
            Ok(())
        })?
    }

    pub fn lifecycle(&self, unit: UnitId) -> Result<Lifecycle, Error> {
        self.units
            .acquire(|units| units.lifecycle(unit))?
            .map_err(Error::from)
    }

    pub fn finish_end(&self, unit: UnitId) -> Result<(), Error> {
        self.with_unit_room(|units, room| {
            units.lifecycle(unit)?;
            if room.admitted(unit) {
                return Err(Error::Busy);
            }
            // Released admission proves either no entry occurred or the exact
            // machine leave receipt was settled by Room.
            unsafe { units.finish_end(unit) }?;
            Ok(())
        })
    }

    pub fn reap(&self, unit: UnitId) -> Result<End, Error> {
        self.units
            .acquire(|units| units.reap(unit))?
            .map_err(Error::from)
    }

    fn leave(&self, unit: UnitId, end: Option<u64>) -> Result<(), Error> {
        let (_, local) = self.local()?;
        if !treaty::cooperative_ready() {
            return Err(Error::Unsupported);
        }
        if !treaty::can_switch() {
            return Err(Error::Protection);
        }
        let occupancy = local
            .running()
            .filter(|active| active.unit() == unit)
            .ok_or(Error::Gone)?;
        let context = self.with_unit_room(|units, room| {
            let (_, context) = units.live(unit)?;
            let destination = if end.is_some() {
                LeaveDestination::Release
            } else {
                LeaveDestination::Ready
            };
            room.request_leave(occupancy, destination)?;
            if let Some(value) = end {
                units
                    .begin_end(unit, End::Returned { value })
                    .expect("validated live Unit");
            }
            Ok(context)
        })?;
        // Local protection is settled before transfer. On migration the returning call
        // finishes without accessing the original processor's local state.
        unsafe { local.leave(occupancy, context) };
        Ok(())
    }

    fn end(&self, unit: UnitId, value: u64) -> Result<!, Error> {
        self.leave(unit, Some(value))?;
        self.terminal.reject(format_args!(
            "FAIL Limen execution: terminal continuation resumed\n"
        ))
    }
}

/// The currently running trusted Unit, scoped to its entry and continuation.
pub struct Current {
    execution: &'static Execution,
    unit: UnitId,
    local: PhantomData<*mut ()>,
    #[cfg(target_arch = "riscv64")]
    fund: Option<&'static Fund<'static>>,
}

impl Current {
    pub fn id(&self) -> UnitId {
        self.unit
    }

    /// The hardware identity of the processor executing this continuation.
    pub fn processor(&self) -> usize {
        treaty::try_current_processor()
            .expect("ordinary processor entry")
            .get()
    }

    /// Allow the original accepted local timer to stop this dispatch while
    /// executing in the half-open native instruction span. A native return
    /// outside the span preserves the current Unit; the caller still owns
    /// ordinary progress and any unsettled timer effects.
    ///
    /// # Safety
    /// Every execution at these resident instruction addresses during the
    /// original occupancy must allow abandoning its whole Unit stack without
    /// leaving guards, live external borrows or owed cleanup. The native
    /// unmask must order this publication before an IRQ can be delivered, for
    /// example by using an assembly unmask with a compiler memory clobber.
    /// A switch or terminal return closes this permission.
    #[cfg(target_arch = "riscv64")]
    pub unsafe fn allow_stop_in(&mut self, span: Range<usize>) -> Result<Occupancy, Error> {
        if self.fund.is_some() {
            return Err(Error::Unsupported);
        }
        if !treaty::cooperative_ready() || !treaty::can_switch() {
            return Err(Error::Protection);
        }
        let (_, local) = self.execution.local()?;
        let occupancy = local
            .running()
            .filter(|active| active.unit() == self.unit)
            .ok_or(Error::Gone)?;
        local
            .allow_stop_in(span)
            .then_some(occupancy)
            .ok_or(Error::State)
    }

    /// Yield the processor. Success is returned after a new dispatch resumes this
    /// continuation.
    pub fn yield_now(&mut self) -> Result<(), Error> {
        #[cfg(target_arch = "riscv64")]
        if self.fund.is_some() {
            return Err(Error::Unsupported);
        }
        self.execution.leave(self.unit, None)
    }

    /// Wait for a deadline in this fixed processor's Sign clock domain.
    /// Success follows a trustworthy local sample reaching the original target.
    ///
    /// # Safety
    /// The resident dispatcher retains this exact Sign and its original
    /// comparator. It must be able to service every accepted wait, even
    /// while this Unit is off the processor, and keep a valid native wake path.
    /// For funded execution, the admitted stop bounds must also cover the
    /// complete masked Arm path, including every acquisition and release of
    /// its ordinary Turn and all failure paths to the covered span or actual
    /// departure. A single attempt or Fast mode alone does not prove this
    /// bound. The initial single-processor profile excludes all competing
    /// access to those owners during the masked path.
    pub unsafe fn wait_until<const N: usize>(
        &mut self,
        sign: &Sign<'_, N>,
        deadline: Deadline<'_>,
    ) -> Result<(), Error> {
        #[cfg(target_arch = "riscv64")]
        if let Some(fund) = self.fund {
            fund.check_wait(self, sign, deadline)?;
        }
        self.execution.wait_until(self.unit, sign, deadline)
    }
}

struct Start {
    execution: &'static Execution,
    entry: Entry,
    #[cfg(target_arch = "riscv64")]
    fund: Option<&'static Fund<'static>>,
}

extern "C" fn start(argument: *const ()) -> ! {
    // On first entry this points to the suspended dispatcher's live Start.
    let start = unsafe { argument.cast::<Start>().read() };
    let Start {
        execution, entry, ..
    } = start;
    let (_, local) = execution.local().expect("attached processor");
    let unit = local.running().expect("authorized Kernel Unit").unit();
    let mut current = Current {
        execution,
        unit,
        local: PhantomData,
        #[cfg(target_arch = "riscv64")]
        fund: start.fund,
    };
    let value = entry(&mut current);
    #[cfg(target_arch = "riscv64")]
    if let Some(fund) = start.fund {
        fund.end(&current, value);
    }
    let Err(error) = execution.end(unit, value);
    execution.terminal.reject(format_args!(
        "FAIL Limen execution: terminal leave {error:?}\n"
    ))
}
