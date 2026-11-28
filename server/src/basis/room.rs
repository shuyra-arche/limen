//! Bounded Executor participation, Unit admission and exact execution occupancy.

use core::{
    marker::PhantomData,
    num::NonZeroU64,
    sync::atomic::{
        AtomicU64,
        Ordering::{Acquire, Relaxed, Release},
    },
};

use super::unit::UnitId;
use crate::metal::processor::treaty::{LeaveReceipt, ProcessorId};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Error {
    Busy,
    Capacity,
    Gone,
    State,
    Unsupported,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AdmissionId(NonZeroU64);

// Participants do not detach or reuse slots in the cooperative Room.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ParticipationId(usize);

// This sequence is scoped by the admission in Occupancy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct OccupancyId(NonZeroU64);

/// One dispatch of an admitted Unit on a participating processor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Occupancy {
    id: OccupancyId,
    unit: UnitId,
    admission: AdmissionId,
    processor: ProcessorId,
    participation: ParticipationId,
}

impl Occupancy {
    pub fn unit(self) -> UnitId {
        self.unit
    }

    pub fn admission(self) -> AdmissionId {
        self.admission
    }

    pub fn processor(self) -> usize {
        self.processor.get()
    }
}

#[derive(Clone, Copy)]
pub(crate) enum LeaveDestination {
    Ready,
    Release,
}

#[derive(Clone, Copy)]
enum LeaveRequest {
    Ready(OccupancyId),
    Release,
}

#[derive(Clone, Copy)]
enum Placement {
    Ready(OccupancyId),
    Occupied {
        occupancy: Occupancy,
        leave: Option<LeaveRequest>,
    },
}

struct UnitEntry {
    unit: UnitId,
    admission: AdmissionId,
    affinity: Option<ProcessorId>,
    placement: Placement,
}

struct Participant {
    processor: ProcessorId,
    occupied: Option<usize>,
}

impl Participant {
    const fn new(processor: ProcessorId) -> Self {
        Self {
            processor,
            occupied: None,
        }
    }
}

/// Stable storage for actual Room participants.
pub struct Participants<const CAPACITY: usize> {
    entries: [Option<Participant>; CAPACITY],
    ready_revision: AtomicU64,
}

impl<const CAPACITY: usize> Participants<CAPACITY> {
    pub const fn new() -> Self {
        Self {
            entries: [const { None }; CAPACITY],
            ready_revision: AtomicU64::new(0),
        }
    }

    pub(super) fn accepts(&self) -> bool {
        CAPACITY != 0 && self.entries.iter().all(Option::is_none)
    }

    pub(super) fn room<const UNITS: usize>(&'static mut self) -> Room<UNITS> {
        assert!(self.accepts());
        Room::new(&mut self.entries, &self.ready_revision)
    }
}

impl<const CAPACITY: usize> Default for Participants<CAPACITY> {
    fn default() -> Self {
        Self::new()
    }
}

/// The Room had no compatible Ready Unit for the observing dispatcher.
/// A change requests a fresh observation under Room protection.
pub struct IdleObservation {
    ready_revision: &'static AtomicU64,
    observed: u64,
    local: PhantomData<*mut ()>,
}

impl IdleObservation {
    /// Check for a Ready publication with one atomic read.
    /// Exhaustion conservatively requests a recheck.
    pub fn changed(&self) -> bool {
        self.observed == u64::MAX || self.ready_revision.load(Acquire) != self.observed
    }
}

pub(crate) struct Room<const CAPACITY: usize> {
    units: [Option<UnitEntry>; CAPACITY],
    participants: &'static mut [Option<Participant>],
    // Both discovery indices point to the UnitEntry that owns the placement.
    ready: [usize; CAPACITY],
    ready_len: usize,
    ready_revision: &'static AtomicU64,
    next_admission: Option<AdmissionId>,
}

impl<const CAPACITY: usize> Room<CAPACITY> {
    fn new(
        participants: &'static mut [Option<Participant>],
        ready_revision: &'static AtomicU64,
    ) -> Self {
        Self {
            units: [const { None }; CAPACITY],
            participants,
            ready: [0; CAPACITY],
            ready_len: 0,
            ready_revision,
            next_admission: Some(AdmissionId(NonZeroU64::MIN)),
        }
    }

    /// Called after the trusted entry has established the Executor.
    pub(super) fn attach(&mut self, processor: ProcessorId) -> Result<(), Error> {
        if self.participation(processor).is_some() {
            return Err(Error::Busy);
        }
        let index = self
            .participants
            .iter()
            .position(Option::is_none)
            .ok_or(Error::Capacity)?;
        self.participants[index] = Some(Participant::new(processor));
        Ok(())
    }

    pub(crate) fn submit(
        &mut self,
        unit: UnitId,
        affinity: Option<ProcessorId>,
    ) -> Result<AdmissionId, Error> {
        if self.admitted(unit) {
            return Err(Error::Busy);
        }
        if affinity.is_some_and(|processor| self.participation(processor).is_none()) {
            return Err(Error::Unsupported);
        }
        let slot = self
            .units
            .iter()
            .position(Option::is_none)
            .ok_or(Error::Capacity)?;
        let admission = self.next_admission.ok_or(Error::Capacity)?;
        self.next_admission = admission.0.checked_add(1).map(AdmissionId);
        self.units[slot] = Some(UnitEntry {
            unit,
            admission,
            affinity,
            placement: Placement::Ready(OccupancyId(NonZeroU64::MIN)),
        });
        self.enqueue(slot);
        Ok(admission)
    }

    fn enqueue(&mut self, slot: usize) {
        // Each Ready entry has one index. The admitted slot reserves its space.
        self.ready[self.ready_len] = slot;
        self.ready_len += 1;
        // Room's Turn serializes every Ready publication. Saturation prevents
        // an old idle observation from becoming current again.
        let revision = self.ready_revision.load(Relaxed);
        self.ready_revision
            .store(revision.saturating_add(1), Release);
    }

    pub(crate) fn admitted(&self, unit: UnitId) -> bool {
        self.units.iter().flatten().any(|entry| entry.unit == unit)
    }

    fn participation(&self, processor: ProcessorId) -> Option<ParticipationId> {
        self.participants
            .iter()
            .position(|slot| {
                slot.as_ref()
                    .is_some_and(|attached| attached.processor == processor)
            })
            .map(ParticipationId)
    }

    fn available(&self, processor: ProcessorId) -> Result<ParticipationId, Error> {
        let participation = self.participation(processor).ok_or(Error::Unsupported)?;
        let participant = self.participants[participation.0]
            .as_ref()
            .expect("participant");
        if participant.occupied.is_some() {
            return Err(Error::Busy);
        }
        Ok(participation)
    }

    fn first_ready(&self, processor: ProcessorId) -> Option<(usize, usize)> {
        self.ready[..self.ready_len]
            .iter()
            .copied()
            .enumerate()
            .find(|&(_, slot)| {
                let entry = self.units[slot].as_ref().expect("ready Unit");
                entry.affinity.is_none_or(|target| target == processor)
            })
    }

    pub(crate) fn candidate(&self, processor: ProcessorId) -> Result<Option<UnitId>, Error> {
        self.available(processor)?;
        Ok(self
            .first_ready(processor)
            .map(|(_, slot)| self.units[slot].as_ref().expect("ready Unit").unit))
    }

    pub(super) fn observe_idle(
        &self,
        processor: ProcessorId,
    ) -> Result<Option<IdleObservation>, Error> {
        Ok(self
            .candidate(processor)?
            .is_none()
            .then(|| IdleObservation {
                ready_revision: self.ready_revision,
                observed: self.ready_revision.load(Relaxed),
                local: PhantomData,
            }))
    }

    pub(crate) fn occupy(
        &mut self,
        processor: ProcessorId,
        unit: UnitId,
    ) -> Result<Occupancy, Error> {
        let participation = self.available(processor)?;
        let (index, slot) = self.first_ready(processor).ok_or(Error::State)?;
        let entry = self.units[slot].as_mut().expect("ready Unit");
        if entry.unit != unit {
            return Err(Error::State);
        }
        let Placement::Ready(id) = entry.placement else {
            unreachable!("Ready index points to a ready Unit");
        };
        let occupancy = Occupancy {
            id,
            unit,
            admission: entry.admission,
            processor,
            participation,
        };
        entry.placement = Placement::Occupied {
            occupancy,
            leave: None,
        };
        self.ready.copy_within(index + 1..self.ready_len, index);
        self.ready_len -= 1;
        self.participants[participation.0]
            .as_mut()
            .expect("participant")
            .occupied = Some(slot);
        Ok(occupancy)
    }

    pub(crate) fn check_occupied(&self, occupancy: Occupancy) -> Result<(), Error> {
        self.active(occupancy).map(|_| ())
    }

    fn active(&self, occupancy: Occupancy) -> Result<usize, Error> {
        let slot = self
            .participants
            .get(occupancy.participation.0)
            .and_then(Option::as_ref)
            .and_then(|participant| participant.occupied)
            .ok_or(Error::Gone)?;
        let entry = self.units[slot].as_ref().expect("occupied Unit");
        match entry.placement {
            Placement::Occupied {
                occupancy: current, ..
            } if current == occupancy => Ok(slot),
            _ => Err(Error::Gone),
        }
    }

    pub(crate) fn request_leave(
        &mut self,
        occupancy: Occupancy,
        destination: LeaveDestination,
    ) -> Result<(), Error> {
        let slot = self.active(occupancy)?;
        let entry = self.units[slot].as_mut().expect("occupied Unit");
        let Placement::Occupied { leave, .. } = &mut entry.placement else {
            unreachable!("validated occupancy");
        };
        if leave.is_some() {
            return Err(Error::Busy);
        }
        *leave = Some(match destination {
            LeaveDestination::Ready => LeaveRequest::Ready(OccupancyId(
                // Exhaustion still permits a terminal leave from this occupancy.
                occupancy.id.0.checked_add(1).ok_or(Error::Capacity)?,
            )),
            LeaveDestination::Release => LeaveRequest::Release,
        });
        Ok(())
    }

    pub(crate) fn vacate(&mut self, receipt: LeaveReceipt<Occupancy>) -> Result<(), Error> {
        let occupancy = receipt.occupancy();
        let slot = self.active(occupancy)?;
        let entry = self.units[slot].as_mut().expect("occupied Unit");
        let Placement::Occupied { leave, .. } = entry.placement else {
            unreachable!("validated occupancy");
        };
        match leave.ok_or(Error::State)? {
            LeaveRequest::Ready(next) => {
                entry.placement = Placement::Ready(next);
                self.enqueue(slot);
            }
            LeaveRequest::Release => self.units[slot] = None,
        }
        self.participants[occupancy.participation.0]
            .as_mut()
            .expect("participant")
            .occupied = None;
        Ok(())
    }
}
