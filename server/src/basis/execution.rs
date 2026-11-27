//! Coordinates the existing cooperative Unit, Room and machine handoff.

use core::{fmt::Write, marker::PhantomData};

use crate::{
    basis::{
        room::{self, AdmissionId, Harts, LeaveDestination, Occupancy, Room},
        unit::{self, End, Lifecycle, STACK_CAPACITY, Stacks, UnitId, UnitRecord, Units},
    },
    metal::{
        processor::execution::cooperative::{self, Hart, LeaveReceipt, LocalHart, cooperative_ready},
        processor::hart::local::{self, HartId},
        sync::{TurnAcquireError, TurnLock},
    },
};

use crate::entry::boot::{self, Boot};

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EstablishError {
    WrongHart,
    Unsupported,
    Storage,
}

/// The original handoff and independent supplies, returned before ownership.
pub struct EstablishFailure<'a, const HARTS: usize> {
    pub boot: Boot<'a>,
    pub stacks: &'static mut Stacks,
    pub records: &'static mut [UnitRecord<KernelBinding>],
    pub harts: &'static mut Harts<HARTS>,
    pub reason: EstablishError,
}

/// The trusted cooperative Unit entry and its actual placement constraint.
pub struct KernelBinding {
    entry: Entry,
    affinity: Option<HartId>,
}

struct Dispatch {
    units: Units<KernelBinding>,
    room: Room<STACK_CAPACITY>,
}

/// Cooperative coordination of Unit and Room on independently attached Harts.
pub struct Execution {
    dispatch: TurnLock<Dispatch>,
    harts: &'static [Hart<Occupancy>],
}

impl Execution {
    /// Consume the unique trusted construction handoff. No Hart participates
    /// until its own Attach; all supplies remain independent and resident.
    pub fn new<'a, const HARTS: usize>(
        boot: Boot<'a>,
        stacks: &'static mut Stacks,
        records: &'static mut [UnitRecord<KernelBinding>],
        harts: &'static mut Harts<HARTS>,
    ) -> Result<Self, EstablishFailure<'a, HARTS>> {
        let reason = if local::try_current_hart() != Some(HartId::new(boot.hart())) {
            Some(EstablishError::WrongHart)
        } else if !cooperative_ready() {
            Some(EstablishError::Unsupported)
        } else if !Units::accepts(records) || !harts.accepts() {
            Some(EstablishError::Storage)
        } else {
            None
        };
        if let Some(reason) = reason {
            return Err(EstablishFailure {
                boot,
                stacks,
                records,
                harts,
                reason,
            });
        }
        let (room, harts) = harts.split();
        Ok(Self {
            dispatch: TurnLock::new(Dispatch {
                units: Units::new(stacks, records),
                room,
            }),
            harts,
        })
    }

    fn dispatch<R>(
        &self,
        operation: impl FnOnce(&mut Dispatch) -> Result<R, Error>,
    ) -> Result<R, Error> {
        self.dispatch.acquire(operation)?
    }

    fn local(&self) -> Result<(HartId, LocalHart<'_, Occupancy>), Error> {
        local::try_current_hart().ok_or(Error::Protection)?;
        cooperative::current(self.harts).ok_or(Error::Unsupported)
    }

    /// Attach this actual Hart after its trusted entry. This is independent
    /// of firmware launch; every participant takes the same path.
    pub fn attach(&self) -> Result<(), Error> {
        let id = local::try_current_hart().ok_or(Error::Protection)?;
        if !cooperative_ready() {
            return Err(Error::Unsupported);
        }
        if !local::can_switch() {
            return Err(Error::Protection);
        }
        self.dispatch(|dispatch| {
            if cooperative::attached() {
                return Err(Error::Busy);
            }
            let participation = dispatch.room.attach(id)?;
            // All fallible admission is complete. Only this Hart uses its
            // local binding, and Room retains this slot for the entire prefix.
            unsafe { cooperative::bind(self.harts, participation.index()) };
            Ok(())
        })
    }

    /// Construct a dormant Kernel Unit. Its entry's return value is its End.
    ///
    /// # Safety
    ///
    /// The caller supplies a trusted supervisor entry whose compiler and
    /// callees fit the 16 KiB stack, preserve the established translation and trap
    /// configuration, Hart-local registers, and the selected backend's
    /// interrupt and extension restrictions. Any local borrow must allow suspension at
    /// `yield_now`. The Unit stays on the constructing Hart.
    pub unsafe fn create(&self, entry: Entry) -> Result<UnitId, Error> {
        let (hart, _) = self.local()?;
        self.create_unit(entry, Some(hart))
    }

    /// Construct a Unit eligible to resume on any attached compatible Hart.
    ///
    /// # Safety
    ///
    /// The entry must satisfy `create` except for affinity. Every live value
    /// at Yield must allow transfer to another Hart. The entry must release
    /// Hart-local borrows and protection before Yield and reacquire local state
    /// on the destination Hart.
    pub unsafe fn create_migratable(&self, entry: Entry) -> Result<UnitId, Error> {
        self.local()?;
        self.create_unit(entry, None)
    }

    fn create_unit(&self, entry: Entry, affinity: Option<HartId>) -> Result<UnitId, Error> {
        self.dispatch(|dispatch| {
            let unit = dispatch.units.vacant()?;
            dispatch
                .units
                .publish(unit, KernelBinding { entry, affinity }, start)?;
            Ok(unit)
        })
    }

    pub fn submit(&self, unit: UnitId) -> Result<AdmissionId, Error> {
        self.local()?;
        self.dispatch(|dispatch| {
            let affinity = dispatch.units.live(unit)?.0.affinity;
            dispatch.room.submit(unit, affinity).map_err(Error::from)
        })
    }

    /// Select cancellation for a Unit that has never been admitted to Room.
    /// A successful submission wins against this operation. After a
    /// cancellation, use `finish_end` and `reap` to release the slot.
    pub fn stop_dormant(&self, unit: UnitId) -> Result<(), Error> {
        self.local()?;
        self.dispatch(|dispatch| {
            if dispatch.room.admitted(unit) {
                return Err(Error::Busy);
            }
            dispatch
                .units
                .begin_end(unit, End::Cancelled)
                .map_err(Error::from)
        })
    }

    /// Run one ready Unit until it yields or returns on its own stack.
    /// The original admission remains occupied until `vacate`.
    pub fn enter(&'static self) -> Result<Option<Occupancy>, Error> {
        let (id, hart) = self.local()?;
        if !cooperative_ready() {
            return Err(Error::Unsupported);
        }
        if !local::can_switch() {
            return Err(Error::Protection);
        }
        let Some((occupancy, context, entry)) = self.dispatch(|dispatch| {
            let Some(unit) = dispatch.room.candidate(id)? else {
                return Ok(None);
            };
            let (binding, context) = dispatch.units.live(unit)?;
            let entry = binding.entry;
            Ok(Some((dispatch.room.occupy(id, unit)?, context, entry)))
        })?
        else {
            return Ok(None);
        };
        let start = Start {
            execution: self,
            entry,
        };
        // Room retains this exact occupancy until this Hart completes the
        // leave. The initial continuation copies Start before it can suspend.
        unsafe { hart.enter(occupancy, context, (&raw const start).cast()) };
        Ok(Some(occupancy))
    }

    pub fn pending_leave(&self) -> Result<Option<Occupancy>, Error> {
        Ok(self.local()?.1.stopped().map(LeaveReceipt::occupancy))
    }

    pub fn vacate(&self, occupancy: Occupancy) -> Result<(), Error> {
        let (_, hart) = self.local()?;
        self.dispatch(|dispatch| {
            dispatch.room.on_hart(occupancy)?;
            let receipt = hart
                .stopped()
                .filter(|receipt| receipt.occupancy() == occupancy)
                .ok_or(Error::Busy)?;
            dispatch.room.vacate(receipt)?;
            hart.settle(receipt);
            Ok(())
        })
    }

    pub fn lifecycle(&self, unit: UnitId) -> Result<Lifecycle, Error> {
        self.local()?;
        self.dispatch(|dispatch| dispatch.units.lifecycle(unit).map_err(Error::from))
    }

    pub fn finish_end(&self, unit: UnitId) -> Result<(), Error> {
        self.local()?;
        self.dispatch(|dispatch| {
            dispatch.units.lifecycle(unit)?;
            if dispatch.room.admitted(unit) {
                return Err(Error::Busy);
            }
            // Released admission proves either no entry occurred or the exact
            // machine leave receipt was settled by Room.
            unsafe { dispatch.units.finish_end(unit) }?;
            Ok(())
        })
    }

    pub fn reap(&self, unit: UnitId) -> Result<End, Error> {
        self.local()?;
        self.dispatch(|dispatch| dispatch.units.reap(unit).map_err(Error::from))
    }

    fn leave(&self, unit: UnitId, end: Option<u64>) -> Result<(), Error> {
        let (_, hart) = self.local()?;
        if !cooperative_ready() {
            return Err(Error::Unsupported);
        }
        if !local::can_switch() {
            return Err(Error::Protection);
        }
        let occupancy = hart
            .running()
            .filter(|active| active.unit() == unit)
            .ok_or(Error::Gone)?;
        let context = self.dispatch(|dispatch| {
            let (_, context) = dispatch.units.live(unit)?;
            let destination = if end.is_some() {
                LeaveDestination::Release
            } else {
                LeaveDestination::Ready
            };
            dispatch.room.request_leave(occupancy, destination)?;
            if let Some(value) = end {
                dispatch
                    .units
                    .begin_end(unit, End::Returned { value })
                    .expect("validated live Unit");
            }
            Ok(context)
        })?;
        // Local protection is settled before transfer. On migration the returning call
        // finishes without accessing the original Hart's local state.
        unsafe { hart.leave(occupancy, context) };
        Ok(())
    }

    fn end(&self, unit: UnitId, value: u64) -> Result<!, Error> {
        self.leave(unit, Some(value))?;
        boot::write("FAIL Limen execution: terminal continuation resumed\n");
        boot::shutdown(false)
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

    /// The physical Hart executing this continuation when called.
    pub fn hart(&self) -> usize {
        local::current_hart().get()
    }

    /// Yield the Hart. Success is returned after a new dispatch resumes this
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
    let (_, hart) = execution.local().expect("attached Hart");
    let unit = hart.running().expect("authorized Kernel Unit").unit();
    let value = entry(&mut Current {
        execution,
        unit,
        local: PhantomData,
    });
    let Err(error) = execution.end(unit, value);
    if let Some(mut console) = boot::console() {
        let _ = writeln!(console, "FAIL Limen execution: terminal leave {error:?}");
    }
    boot::shutdown(false)
}

