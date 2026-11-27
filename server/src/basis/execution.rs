//! Coordinates the existing cooperative Unit, Room and machine handoff.

use core::marker::PhantomData;

use crate::{
    basis::{
        room::{self, AdmissionId, LeaveDestination, Occupancy, Participants, Room},
        unit::{self, End, Lifecycle, STACK_CAPACITY, Stacks, UnitId, UnitRecord, Units},
    },
    metal::{
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
    State,
    Unsupported,
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

    fn local(&self) -> Result<(ProcessorId, LocalExecution<'_, Occupancy>), Error> {
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
        self.units.acquire(|units| {
            let unit = units.vacant()?;
            units.publish(unit, KernelBinding { entry, affinity }, start)?;
            Ok(unit)
        })?
    }

    pub fn submit(&self, unit: UnitId) -> Result<AdmissionId, Error> {
        self.with_unit_room(|units, room| {
            let affinity = units.live(unit)?.0.affinity;
            room.submit(unit, affinity).map_err(Error::from)
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

    /// Run one ready Unit until it yields or returns on its own stack.
    /// The original admission remains occupied until `vacate`.
    pub fn enter(&'static self) -> Result<Option<Occupancy>, Error> {
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
        };
        // Room retains this exact occupancy until this processor completes the
        // leave. The initial continuation copies Start before it can suspend.
        unsafe { local.enter(occupancy, context, (&raw const start).cast()) };
        Ok(Some(occupancy))
    }

    pub fn pending_leave(&self) -> Result<Option<Occupancy>, Error> {
        Ok(self.local()?.1.stopped().map(LeaveReceipt::occupancy))
    }

    pub fn vacate(&self, occupancy: Occupancy) -> Result<(), Error> {
        let (_, local) = self.local()?;
        self.room.acquire(|room| {
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

    /// Yield the processor. Success is returned after a new dispatch resumes this
    /// continuation.
    pub fn yield_now(&mut self) -> Result<(), Error> {
        self.execution.leave(self.unit, None)
    }
}

struct Start {
    execution: &'static Execution,
    entry: Entry,
}

extern "C" fn start(argument: *const ()) -> ! {
    // On first entry this points to the suspended dispatcher's live Start.
    let Start { execution, entry } = unsafe { argument.cast::<Start>().read() };
    let (_, local) = execution.local().expect("attached processor");
    let unit = local.running().expect("authorized Kernel Unit").unit();
    let value = entry(&mut Current {
        execution,
        unit,
        local: PhantomData,
    });
    let Err(error) = execution.end(unit, value);
    execution.terminal.reject(format_args!(
        "FAIL Limen execution: terminal leave {error:?}\n"
    ))
}
