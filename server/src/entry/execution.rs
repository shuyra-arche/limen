//! Trusted entry and handoff for cooperative Kernel Units in the Bare Nest.

use core::{
    fmt::Write,
    marker::PhantomData,
    ptr,
    sync::atomic::{
        AtomicUsize,
        Ordering::{Acquire, Release},
    },
};

pub use crate::basis::{
    nest::Stacks,
    room::Occupancy,
    unit::{End, Lifecycle, UnitId},
};
use crate::{
    basis::{
        nest::{KernelNest, Storage},
        room::{self, Destination, Room},
        unit::{self, Units},
    },
    metal::{
        execution::{Hart, LeaveReceipt, LocalHart},
        hart,
        sync::{TurnAcquireError, TurnLock},
    },
};

use super::boot::{self, Boot, Minor};

type Entry = fn(&mut Current) -> u64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Busy,
    Capacity,
    Gone,
    State,
    Unsupported,
    Firmware(isize),
}

impl From<unit::Error> for Error {
    fn from(error: unit::Error) -> Self {
        match error {
            unit::Error::Capacity => Self::Capacity,
            unit::Error::Gone => Self::Gone,
            unit::Error::State => Self::State,
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
            TurnAcquireError::Context | TurnAcquireError::Empty => Self::State,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EstablishError {
    WrongHart,
    Unsupported,
}

/// The original handoff and stack supply, returned before execution takes ownership.
pub struct EstablishFailure<'a> {
    pub boot: Boot<'a>,
    pub stacks: &'static mut Stacks,
    pub reason: EstablishError,
}

struct Binding {
    storage: &'static Storage,
    entry: Entry,
    affinity: Option<usize>,
}

struct Control {
    units: Units<Binding>,
    room: Room,
    launch: Option<Launch>,
}

#[derive(Clone, Copy)]
struct Launch {
    hart: usize,
    entry: fn(&'static Execution) -> !,
}

/// Cooperative execution on the Boot Hart and one optional minor Hart.
pub struct Execution {
    control: TurnLock<Control>,
    nest: KernelNest,
    major: Hart,
    major_id: usize,
    minor: Hart,
    minor_id: AtomicUsize,
    minor_boot: Option<&'static Minor>,
}

impl Execution {
    /// Consume the trusted handoff and provision the resident stack supply.
    pub fn new<'a>(
        boot: Boot<'a>,
        stacks: &'static mut Stacks,
    ) -> Result<Self, EstablishFailure<'a>> {
        Self::establish(boot, stacks, None)
    }

    /// Provision the same Kernel Nest with an optional firmware-started Hart.
    pub fn with_minor<'a>(
        boot: Boot<'a>,
        stacks: &'static mut Stacks,
        minor: &'static Minor,
    ) -> Result<Self, EstablishFailure<'a>> {
        Self::establish(boot, stacks, Some(minor))
    }

    fn establish<'a>(
        boot: Boot<'a>,
        stacks: &'static mut Stacks,
        minor: Option<&'static Minor>,
    ) -> Result<Self, EstablishFailure<'a>> {
        if hart::current_hart() != boot.hart() {
            return Err(EstablishFailure {
                boot,
                stacks,
                reason: EstablishError::WrongHart,
            });
        }
        if !Hart::supported() {
            return Err(EstablishFailure {
                boot,
                stacks,
                reason: EstablishError::Unsupported,
            });
        }
        Ok(Self {
            control: TurnLock::new(Control {
                units: Units::new(),
                room: Room::new(boot.hart()),
                launch: None,
            }),
            nest: KernelNest::bare(stacks),
            major: Hart::new(),
            major_id: boot.hart(),
            minor: Hart::new(),
            minor_id: AtomicUsize::new(0),
            minor_boot: minor,
        })
    }

    fn control<R>(
        &self,
        operation: impl FnOnce(&mut Control) -> Result<R, Error>,
    ) -> Result<R, Error> {
        self.control.acquire(operation)?
    }

    fn local(&self) -> Result<(usize, LocalHart<'_>), Error> {
        let id = hart::current_hart();
        if id == self.major_id {
            return Ok((id, unsafe { self.major.local() }));
        }
        let minor = self.minor_id.load(Acquire);
        if minor != 0 && id == minor - 1 {
            return Ok((id, unsafe { self.minor.local() }));
        }
        Err(Error::Unsupported)
    }

    /// Start the minor Hart through OpenSBI. It attaches on its dispatcher
    /// stack with the early trap vector installed before the platform entry.
    ///
    /// # Safety
    ///
    /// The entry must be trusted supervisor code built for the Bare Nest.
    /// It must preserve gp, tp, masked interrupts, disabled extensions and
    /// the configured traps. The 32 KiB dispatcher stack is shared by the
    /// live Limen startup frames, the entry and all dispatcher-side callees.
    /// The caller must include those startup frames in the total stack
    /// budget when bounding the entry and its callees.
    pub unsafe fn start_minor(
        &'static self,
        hart: usize,
        entry: fn(&'static Execution) -> !,
    ) -> Result<(), Error> {
        let minor = self.minor_boot.ok_or(Error::Unsupported)?;
        if hart::current_hart() != self.major_id || !Hart::supported() {
            return Err(Error::Unsupported);
        }
        if hart == self.major_id {
            return Err(Error::Busy);
        }
        hart.checked_add(1).ok_or(Error::Unsupported)?;
        self.control(|control| {
            if control.launch.is_some() {
                return Err(Error::Busy);
            }
            control.launch = Some(Launch { hart, entry });
            Ok(())
        })?;
        let argument = ptr::from_ref(self).expose_provenance();
        if let Err(error) = unsafe { minor.start(hart, admit_minor, argument, boot::Startup::Sbi) }
        {
            self.control(|control| {
                control.launch = None;
                Ok(())
            })?;
            return Err(match error {
                boot::StartError::Busy => Error::Busy,
                boot::StartError::Rejected(code) => Error::Firmware(code),
            });
        }
        Ok(())
    }

    /// Construct a dormant Kernel Unit. Its entry's return value is its End.
    ///
    /// # Safety
    ///
    /// The caller supplies a trusted supervisor entry whose compiler and
    /// callees fit the 16 KiB stack, preserve the established Bare and trap
    /// configuration and Hart-local registers, and run with interrupts and
    /// FP/vector state disabled. Any local borrow must allow suspension at
    /// `yield_now`. The Unit stays on the constructing Hart.
    pub unsafe fn create(&self, entry: Entry) -> Result<UnitId, Error> {
        let (hart, _) = self.local()?;
        self.create_unit(entry, Some(hart))
    }

    /// Construct a Unit eligible to resume on either attached Hart.
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

    fn create_unit(&self, entry: Entry, affinity: Option<usize>) -> Result<UnitId, Error> {
        self.control(|control| {
            let unit = control.units.vacant()?;
            let storage = self.nest.bind(unit.slot());
            // The vacant generation excludes every prior stack user.
            unsafe { storage.context().prepare(storage.stack_top(), start) };
            control.units.publish(
                unit,
                Binding {
                    storage,
                    entry,
                    affinity,
                },
            );
            Ok(unit)
        })
    }

    pub fn submit(&self, unit: UnitId) -> Result<(), Error> {
        self.local()?;
        self.control(|control| {
            let affinity = control.units.live(unit)?.affinity;
            control.room.submit(unit, affinity).map_err(Error::from)
        })
    }

    /// Select cancellation for a Unit that has never been admitted to Room.
    /// A successful submission wins against this operation. After a
    /// cancellation, use `finish_end` and `reap` to release the slot.
    pub fn stop_dormant(&self, unit: UnitId) -> Result<(), Error> {
        self.local()?;
        self.control(|control| {
            if control.room.admitted(unit) {
                return Err(Error::Busy);
            }
            control
                .units
                .begin_end(unit, End::Cancelled)
                .map_err(Error::from)
        })
    }

    /// Run one ready Unit until it yields or returns on its own stack.
    /// The original admission remains occupied until `vacate`.
    pub fn enter(&'static self) -> Result<Option<Occupancy>, Error> {
        let (id, hart) = self.local()?;
        if !Hart::supported() {
            return Err(Error::Unsupported);
        }
        if !hart::can_switch() {
            return Err(Error::State);
        }
        let Some((occupancy, storage, entry)) = self.control(|control| {
            let Some(unit) = control.room.candidate(id)? else {
                return Ok(None);
            };
            let binding = control.units.live(unit)?;
            let (storage, entry) = (binding.storage, binding.entry);
            Ok(Some((control.room.occupy(id, unit)?, storage, entry)))
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
        unsafe { hart.enter(occupancy, storage.context(), (&raw const start).cast()) };
        Ok(Some(occupancy))
    }

    pub fn pending_leave(&self) -> Result<Option<Occupancy>, Error> {
        Ok(self.local()?.1.stopped().map(LeaveReceipt::occupancy))
    }

    pub fn vacate(&self, occupancy: Occupancy) -> Result<(), Error> {
        let (_, hart) = self.local()?;
        self.control(|control| {
            control.room.on_hart(occupancy)?;
            let receipt = hart
                .stopped()
                .filter(|receipt| receipt.occupancy() == occupancy)
                .ok_or(Error::Busy)?;
            control.room.vacate(receipt)?;
            hart.settle(receipt);
            Ok(())
        })
    }

    pub fn lifecycle(&self, unit: UnitId) -> Result<Lifecycle, Error> {
        self.local()?;
        self.control(|control| control.units.lifecycle(unit).map_err(Error::from))
    }

    pub fn finish_end(&self, unit: UnitId) -> Result<(), Error> {
        self.local()?;
        self.control(|control| {
            control.units.lifecycle(unit)?;
            if control.room.admitted(unit) {
                return Err(Error::Busy);
            }
            if let Some(binding) = control.units.finish_end(unit)? {
                // Clear the old frame before another Hart can observe Ended
                // and initialize the slot for a different generation.
                unsafe { binding.storage.context().retire() };
            }
            Ok(())
        })
    }

    pub fn reap(&self, unit: UnitId) -> Result<End, Error> {
        self.local()?;
        self.control(|control| control.units.reap(unit).map_err(Error::from))
    }

    fn leave(&self, unit: UnitId, end: Option<u64>) -> Result<(), Error> {
        let (_, hart) = self.local()?;
        if !Hart::supported() {
            return Err(Error::Unsupported);
        }
        if !hart::can_switch() {
            return Err(Error::State);
        }
        let occupancy = hart
            .running()
            .filter(|active| active.unit() == unit)
            .ok_or(Error::Gone)?;
        let storage = self.control(|control| {
            let storage = control.units.live(unit)?.storage;
            let destination = if end.is_some() {
                Destination::Release
            } else {
                Destination::Ready
            };
            control.room.request_leave(occupancy, destination)?;
            if let Some(value) = end {
                control
                    .units
                    .begin_end(unit, End::Returned { value })
                    .expect("validated live Unit");
            }
            Ok(storage)
        })?;
        // Local protection is settled before transfer. On migration the returning call
        // finishes without accessing the original Hart's local state.
        unsafe { hart.leave(occupancy, storage.context()) };
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
        hart::current_hart()
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

fn admit_minor(hart: usize, argument: usize) -> ! {
    // The firmware launch holds a resident Execution for this callback.
    let execution = unsafe { &*ptr::with_exposed_provenance::<Execution>(argument) };
    assert_eq!(hart::current_hart(), hart);
    assert!(Hart::supported());
    let entry = execution
        .control(|control| {
            let launch = control.launch.expect("reserved minor launch");
            assert_eq!(launch.hart, hart);
            control.room.attach(hart)?;
            Ok(launch.entry)
        })
        .expect("reserved Room participation");
    execution.minor_id.store(hart + 1, Release);
    entry(execution)
}
