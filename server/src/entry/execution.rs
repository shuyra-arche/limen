//! Trusted entry and handoff for the first Kernel Units on the Boot Hart.

use core::cell::{RefCell, RefMut};

pub use crate::basis::{
    nest::Stacks,
    room::Occupancy,
    unit::{Error, Lifecycle, UnitId},
};
use crate::{
    basis::{
        nest::{KernelNest, Storage},
        room::{Destination, Room},
        unit::Units,
    },
    metal::riscv::execution::{Hart, LeaveReceipt},
};

use super::boot::{self, Boot};

type Entry = fn(&mut Current) -> u64;

struct Binding {
    storage: &'static Storage,
    entry: Entry,
}

// This protection covers only the joint Unit/Room facts on the Boot Hart.
// It cannot be held across a switch, and the scope never calls into a Unit.
struct Control {
    units: Units<Binding>,
    room: Room,
}

/// Cooperative Kernel Unit execution in the Bare Kernel Nest. This owner
/// and its continuations belong to the single Boot Hart.
pub struct Execution {
    control: RefCell<Control>,
    nest: KernelNest,
    hart: Hart,
}

impl Execution {
    /// Consume the trusted handoff and provision the resident stack supply.
    pub fn new(boot: Boot, stacks: &'static mut Stacks) -> Result<Self, Error> {
        if !Hart::supported() {
            return Err(Error::Unsupported);
        }
        Ok(Self {
            control: RefCell::new(Control {
                units: Units::new(),
                room: Room::new(boot.hart()),
            }),
            nest: KernelNest::bare(stacks),
            hart: Hart::new(),
        })
    }

    fn control(&self) -> Result<RefMut<'_, Control>, Error> {
        self.control.try_borrow_mut().map_err(|_| Error::Busy)
    }

    /// Construct a dormant Kernel Unit. Its entry's return value is its End.
    ///
    /// # Safety
    ///
    /// The caller supplies a trusted supervisor entry whose compiler and
    /// callees fit the 16 KiB stack, preserve the established Bare and trap
    /// configuration, and run with interrupts and FP/vector state disabled.
    /// Any execution-local borrow must allow suspension at `yield_now`.
    pub unsafe fn create(&self, entry: Entry) -> Result<UnitId, Error> {
        let mut control = self.control()?;
        let unit = control.units.vacant()?;
        let storage = self.nest.bind(unit.slot());
        // The vacant generation excludes every prior stack user.
        unsafe { storage.context().prepare(storage.stack_top(), start) };
        control.units.publish(unit, Binding { storage, entry });
        Ok(unit)
    }

    pub fn submit(&self, unit: UnitId) -> Result<(), Error> {
        let mut control = self.control()?;
        control.units.live(unit)?;
        control.room.submit(unit)
    }

    /// Run one ready Unit until it yields or returns on its own stack.
    /// The original admission remains occupied until `vacate`.
    pub fn enter(&'static self) -> Result<Option<Occupancy>, Error> {
        if !Hart::supported() {
            return Err(Error::Unsupported);
        }
        let (occupancy, storage, entry) = {
            let mut control = self.control()?;
            let Some(unit) = control.room.candidate()? else {
                return Ok(None);
            };
            let binding = control.units.live(unit)?;
            let (storage, entry) = (binding.storage, binding.entry);
            (control.room.occupy(unit)?, storage, entry)
        };
        let start = Start {
            execution: self,
            entry,
        };
        // No other Hart or interrupt can intervene between Occupy and entry.
        // The first continuation copies Start before it can suspend.
        unsafe {
            self.hart
                .enter(occupancy, storage.context(), (&raw const start).cast())
        };
        Ok(Some(occupancy))
    }

    pub fn pending_leave(&self) -> Option<Occupancy> {
        self.hart.stopped().map(LeaveReceipt::occupancy)
    }

    pub fn vacate(&self, occupancy: Occupancy) -> Result<(), Error> {
        let mut control = self.control()?;
        control.room.on_hart(occupancy)?;
        let receipt = self
            .hart
            .stopped()
            .filter(|receipt| receipt.occupancy() == occupancy)
            .ok_or(Error::Busy)?;
        control.room.vacate(receipt)?;
        self.hart.settle(receipt);
        Ok(())
    }

    pub fn lifecycle(&self, unit: UnitId) -> Result<Lifecycle, Error> {
        self.control()?.units.lifecycle(unit)
    }

    pub fn finish_end(&self, unit: UnitId) -> Result<(), Error> {
        let binding = {
            let mut control = self.control()?;
            control.units.lifecycle(unit)?;
            if control.room.admitted(unit) {
                return Err(Error::Busy);
            }
            control.units.finish_end(unit)?
        };
        if let Some(binding) = binding {
            // Room released the admission, including its last actual leave.
            unsafe { binding.storage.context().retire() };
        }
        Ok(())
    }

    pub fn reap(&self, unit: UnitId) -> Result<u64, Error> {
        self.control()?.units.reap(unit)
    }

    fn leave(&self, unit: UnitId, end: Option<u64>) -> Result<(), Error> {
        if !Hart::supported() {
            return Err(Error::Unsupported);
        }
        let occupancy = self
            .hart
            .running()
            .filter(|active| active.unit() == unit)
            .ok_or(Error::Gone)?;
        let storage = {
            let mut control = self.control()?;
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
                    .begin_end(unit, value)
                    .expect("validated live Unit");
            }
            storage
        };
        // The caller owns this stack and published the exact leave request.
        unsafe { self.hart.leave(occupancy, storage.context()) };
        Ok(())
    }
}

/// The currently running trusted Unit, scoped to its entry and continuation.
pub struct Current {
    execution: &'static Execution,
    unit: UnitId,
}

impl Current {
    pub fn id(&self) -> UnitId {
        self.unit
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
    let unit = execution
        .hart
        .running()
        .expect("authorized Kernel Unit")
        .unit();
    let value = entry(&mut Current { execution, unit });
    let _ = execution.leave(unit, Some(value));
    boot::write("FAIL Limen execution: terminal continuation did not leave\n");
    boot::shutdown(false)
}
