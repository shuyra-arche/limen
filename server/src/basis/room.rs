//! Bounded Hart participation, Unit admission and exact execution occupancy.

use core::num::NonZeroU64;

use super::unit::UnitId;
use crate::metal::processor::treaty::{HartId, LeaveReceipt};

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

/// One dispatch of an admitted Unit on a participating Hart.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Occupancy {
    id: OccupancyId,
    unit: UnitId,
    admission: AdmissionId,
    hart: HartId,
    participation: ParticipationId,
}

impl Occupancy {
    pub fn unit(self) -> UnitId {
        self.unit
    }

    pub fn admission(self) -> AdmissionId {
        self.admission
    }

    pub fn hart(self) -> usize {
        self.hart.get()
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
    OnHart {
        occupancy: Occupancy,
        leave: Option<LeaveRequest>,
    },
}

struct UnitEntry {
    unit: UnitId,
    admission: AdmissionId,
    affinity: Option<HartId>,
    placement: Placement,
}

struct HartEntry {
    hart: HartId,
    occupied: Option<usize>,
}

impl HartEntry {
    const fn new(hart: HartId) -> Self {
        Self {
            hart,
            occupied: None,
        }
    }
}

/// Stable storage for actual Room participants.
pub struct Harts<const CAPACITY: usize> {
    entries: [Option<HartEntry>; CAPACITY],
}

impl<const CAPACITY: usize> Harts<CAPACITY> {
    pub const fn new() -> Self {
        Self {
            entries: [const { None }; CAPACITY],
        }
    }

    pub(super) fn accepts(&self) -> bool {
        CAPACITY != 0 && self.entries.iter().all(Option::is_none)
    }

    pub(super) fn room<const UNITS: usize>(&'static mut self) -> Room<UNITS> {
        assert!(self.accepts());
        Room::new(&mut self.entries)
    }
}

impl<const CAPACITY: usize> Default for Harts<CAPACITY> {
    fn default() -> Self {
        Self::new()
    }
}

pub(crate) struct Room<const CAPACITY: usize> {
    units: [Option<UnitEntry>; CAPACITY],
    harts: &'static mut [Option<HartEntry>],
    // Both discovery indices point to the UnitEntry that owns the placement.
    ready: [usize; CAPACITY],
    ready_len: usize,
    next_admission: Option<AdmissionId>,
}

impl<const CAPACITY: usize> Room<CAPACITY> {
    fn new(harts: &'static mut [Option<HartEntry>]) -> Self {
        Self {
            units: [const { None }; CAPACITY],
            harts,
            ready: [0; CAPACITY],
            ready_len: 0,
            next_admission: Some(AdmissionId(NonZeroU64::MIN)),
        }
    }

    /// Called after the trusted entry has established the Hart's local state.
    pub(super) fn attach(&mut self, hart: HartId) -> Result<(), Error> {
        if self.participation(hart).is_some() {
            return Err(Error::Busy);
        }
        let index = self
            .harts
            .iter()
            .position(Option::is_none)
            .ok_or(Error::Capacity)?;
        self.harts[index] = Some(HartEntry::new(hart));
        Ok(())
    }

    pub(crate) fn submit(
        &mut self,
        unit: UnitId,
        affinity: Option<HartId>,
    ) -> Result<AdmissionId, Error> {
        if self.admitted(unit) {
            return Err(Error::Busy);
        }
        if affinity.is_some_and(|hart| self.participation(hart).is_none()) {
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
    }

    pub(crate) fn admitted(&self, unit: UnitId) -> bool {
        self.units.iter().flatten().any(|entry| entry.unit == unit)
    }

    fn participation(&self, hart: HartId) -> Option<ParticipationId> {
        self.harts
            .iter()
            .position(|slot| slot.as_ref().is_some_and(|attached| attached.hart == hart))
            .map(ParticipationId)
    }

    fn available(&self, hart: HartId) -> Result<ParticipationId, Error> {
        let participation = self.participation(hart).ok_or(Error::Unsupported)?;
        let participant = self.harts[participation.0].as_ref().expect("attached Hart");
        if participant.occupied.is_some() {
            return Err(Error::Busy);
        }
        Ok(participation)
    }

    fn first_ready(&self, hart: HartId) -> Option<(usize, usize)> {
        self.ready[..self.ready_len]
            .iter()
            .copied()
            .enumerate()
            .find(|&(_, slot)| {
                let entry = self.units[slot].as_ref().expect("ready Unit");
                entry.affinity.is_none_or(|target| target == hart)
            })
    }

    pub(crate) fn candidate(&self, hart: HartId) -> Result<Option<UnitId>, Error> {
        self.available(hart)?;
        Ok(self
            .first_ready(hart)
            .map(|(_, slot)| self.units[slot].as_ref().expect("ready Unit").unit))
    }

    pub(crate) fn occupy(&mut self, hart: HartId, unit: UnitId) -> Result<Occupancy, Error> {
        let participation = self.available(hart)?;
        let (index, slot) = self.first_ready(hart).ok_or(Error::State)?;
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
            hart,
            participation,
        };
        entry.placement = Placement::OnHart {
            occupancy,
            leave: None,
        };
        self.ready.copy_within(index + 1..self.ready_len, index);
        self.ready_len -= 1;
        self.harts[participation.0]
            .as_mut()
            .expect("attached Hart")
            .occupied = Some(slot);
        Ok(occupancy)
    }

    pub(crate) fn on_hart(&self, occupancy: Occupancy) -> Result<(), Error> {
        self.active(occupancy).map(|_| ())
    }

    fn active(&self, occupancy: Occupancy) -> Result<usize, Error> {
        let slot = self
            .harts
            .get(occupancy.participation.0)
            .and_then(Option::as_ref)
            .and_then(|hart| hart.occupied)
            .ok_or(Error::Gone)?;
        let entry = self.units[slot].as_ref().expect("occupied Unit");
        match entry.placement {
            Placement::OnHart {
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
        let Placement::OnHart { leave, .. } = &mut entry.placement else {
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
        let Placement::OnHart { leave, .. } = entry.placement else {
            unreachable!("validated occupancy");
        };
        match leave.ok_or(Error::State)? {
            LeaveRequest::Ready(next) => {
                entry.placement = Placement::Ready(next);
                self.enqueue(slot);
            }
            LeaveRequest::Release => self.units[slot] = None,
        }
        self.harts[occupancy.participation.0]
            .as_mut()
            .expect("attached Hart")
            .occupied = None;
        Ok(())
    }
}
