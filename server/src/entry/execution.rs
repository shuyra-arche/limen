//! Trusted entry and handoff for cooperative Kernel Units in the Bare Nest.

use core::{
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
    unit::{End, Error, Lifecycle, UnitId},
};
use crate::{
    basis::{
        nest::{KernelNest, Storage},
        room::{Destination, Room},
        sync::{Guard, Lock},
        unit::Units,
    },
    metal::{
        boot as hardware,
        execution::{Hart, LeaveReceipt, LocalHart},
    },
};

use super::boot::{self, Boot, Minor};

type Entry = fn(&mut Current) -> u64;

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
    control: Lock<Control>,
    nest: KernelNest,
    major: Hart,
    major_id: usize,
    minor: Hart,
    minor_id: AtomicUsize,
    minor_boot: Option<&'static Minor>,
}

impl Execution {
    /// Consume the trusted handoff and provision the resident stack supply.
    pub fn new(boot: Boot, stacks: &'static mut Stacks) -> Result<Self, Error> {
        Self::establish(boot, stacks, None)
    }

    /// Provision the same Kernel Nest with an optional firmware-started Hart.
    pub fn with_minor(
        boot: Boot,
        stacks: &'static mut Stacks,
        minor: &'static Minor,
    ) -> Result<Self, Error> {
        Self::establish(boot, stacks, Some(minor))
    }

    fn establish(
        boot: Boot,
        stacks: &'static mut Stacks,
        minor: Option<&'static Minor>,
    ) -> Result<Self, Error> {
        if hardware::current_hart() != boot.hart() || !Hart::supported() {
            return Err(Error::Unsupported);
        }
        Ok(Self {
            control: Lock::new(Control {
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

    fn control(&self) -> Result<Guard<'_, Control>, Error> {
        let (_, hart) = self.local()?;
        self.control.try_lock(hart.guards()).ok_or(Error::Busy)
    }

    fn local(&self) -> Result<(usize, LocalHart<'_>), Error> {
        let id = hardware::current_hart();
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
        if hardware::current_hart() != self.major_id || !Hart::supported() {
            return Err(Error::Unsupported);
        }
        if hart == self.major_id {
            return Err(Error::Busy);
        }
        hart.checked_add(1).ok_or(Error::Unsupported)?;
        {
            let mut control = self.control()?;
            if control.launch.is_some() {
                return Err(Error::Busy);
            }
            control.launch = Some(Launch { hart, entry });
        }
        let argument = ptr::from_ref(self).expose_provenance();
        if let Err(error) = unsafe { minor.start(hart, admit_minor, argument, boot::Startup::Sbi) }
        {
            let (_, hart) = self.local()?;
            self.control.lock(hart.guards()).launch = None;
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
    /// Hart-local borrows and guards before Yield and reacquire local state
    /// on the destination Hart.
    pub unsafe fn create_migratable(&self, entry: Entry) -> Result<UnitId, Error> {
        self.local()?;
        self.create_unit(entry, None)
    }

    fn create_unit(&self, entry: Entry, affinity: Option<usize>) -> Result<UnitId, Error> {
        let mut control = self.control()?;
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
    }

    pub fn submit(&self, unit: UnitId) -> Result<(), Error> {
        let mut control = self.control()?;
        let affinity = control.units.live(unit)?.affinity;
        control.room.submit(unit, affinity)
    }

    /// Select cancellation for a Unit that has never been admitted to Room.
    /// A successful submission wins against this operation. After a
    /// cancellation, use `finish_end` and `reap` to release the slot.
    pub fn stop_dormant(&self, unit: UnitId) -> Result<(), Error> {
        let mut control = self.control()?;
        if control.room.admitted(unit) {
            return Err(Error::Busy);
        }
        control.units.begin_end(unit, End::Cancelled)
    }

    /// Run one ready Unit until it yields or returns on its own stack.
    /// The original admission remains occupied until `vacate`.
    pub fn enter(&'static self) -> Result<Option<Occupancy>, Error> {
        let (id, hart) = self.local()?;
        if !Hart::supported() {
            return Err(Error::Unsupported);
        }
        let (occupancy, storage, entry) = {
            let mut control = self.control()?;
            let Some(unit) = control.room.candidate(id)? else {
                return Ok(None);
            };
            let binding = control.units.live(unit)?;
            let (storage, entry) = (binding.storage, binding.entry);
            (control.room.occupy(id, unit)?, storage, entry)
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

    pub fn pending_leave(&self) -> Option<Occupancy> {
        self.local().ok()?.1.stopped().map(LeaveReceipt::occupancy)
    }

    pub fn vacate(&self, occupancy: Occupancy) -> Result<(), Error> {
        let (_, hart) = self.local()?;
        let mut control = self.control()?;
        control.room.on_hart(occupancy)?;
        let receipt = hart
            .stopped()
            .filter(|receipt| receipt.occupancy() == occupancy)
            .ok_or(Error::Busy)?;
        control.room.vacate(receipt)?;
        hart.settle(receipt);
        Ok(())
    }

    pub fn lifecycle(&self, unit: UnitId) -> Result<Lifecycle, Error> {
        self.control()?.units.lifecycle(unit)
    }

    pub fn finish_end(&self, unit: UnitId) -> Result<(), Error> {
        let mut control = self.control()?;
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
    }

    pub fn reap(&self, unit: UnitId) -> Result<End, Error> {
        self.control()?.units.reap(unit)
    }

    fn leave(&self, unit: UnitId, end: Option<u64>) -> Result<(), Error> {
        let (_, hart) = self.local()?;
        if !Hart::supported() {
            return Err(Error::Unsupported);
        }
        let occupancy = hart
            .running()
            .filter(|active| active.unit() == unit)
            .ok_or(Error::Gone)?;
        let storage = {
            let mut control = if end.is_some() {
                self.control.lock(hart.guards())
            } else {
                self.control()?
            };
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
            storage
        };
        // No guard survives the transfer. On migration the returning call
        // finishes without accessing the original Hart's local state.
        unsafe { hart.leave(occupancy, storage.context()) };
        Ok(())
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
        hardware::current_hart()
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
    let _ = execution.leave(unit, Some(value));
    boot::write("FAIL Limen execution: terminal continuation did not leave\n");
    boot::shutdown(false)
}

fn admit_minor(hart: usize, argument: usize) -> ! {
    // The firmware launch holds a resident Execution for this callback.
    let execution = unsafe { &*ptr::with_exposed_provenance::<Execution>(argument) };
    assert_eq!(hardware::current_hart(), hart);
    assert!(Hart::supported());
    let entry = {
        let local = unsafe { execution.minor.local() };
        let mut control = execution.control.lock(local.guards());
        let launch = control.launch.expect("reserved minor launch");
        assert_eq!(launch.hart, hart);
        control
            .room
            .attach(hart)
            .expect("reserved Room participation");
        launch.entry
    };
    execution.minor_id.store(hart + 1, Release);
    entry(execution)
}
