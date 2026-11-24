//! Bounded Hart participation, Unit admission and exact execution occupancy.

use super::unit::{CAPACITY, Error, UnitId};
use crate::metal::arch::execution::LeaveReceipt;

const HART_CAPACITY: usize = 2;

// Participation slots are assigned once and retained for the Room lifetime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Participation(usize);

/// One dispatch on a participating Hart. The Unit generation also identifies its
/// admission because this Room admits a Unit once, until it ends.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Occupancy {
    unit: UnitId,
    hart: usize,
    participation: Participation,
    sequence: u64,
}

impl Occupancy {
    pub fn unit(self) -> UnitId {
        self.unit
    }

    pub fn hart(self) -> usize {
        self.hart
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Destination {
    Ready,
    Release,
}

#[derive(Clone, Copy)]
struct Ready {
    unit: UnitId,
    affinity: Option<usize>,
    sequence: u64,
}

#[derive(Clone, Copy)]
enum Leave {
    Ready(u64),
    Release,
}

struct OnHart {
    occupancy: Occupancy,
    affinity: Option<usize>,
    leave: Option<Leave>,
}

struct HartEntry {
    id: usize,
    on_hart: Option<OnHart>,
}

impl HartEntry {
    const fn new(id: usize) -> Self {
        Self { id, on_hart: None }
    }
}

pub(crate) struct Room {
    harts: [Option<HartEntry>; HART_CAPACITY],
    // The Ready list and the per-Hart OnHart slots share the admission capacity.
    ready: [Option<Ready>; CAPACITY],
}

impl Room {
    pub(crate) const fn new(hart: usize) -> Self {
        Self {
            harts: [Some(HartEntry::new(hart)), None],
            ready: [None; CAPACITY],
        }
    }

    /// Called after the trusted entry has established the Hart's local state.
    pub(crate) fn attach(&mut self, hart: usize) -> Result<(), Error> {
        if self.participation(hart).is_some() {
            return Err(Error::Busy);
        }
        let slot = self
            .harts
            .iter_mut()
            .find(|slot| slot.is_none())
            .ok_or(Error::Capacity)?;
        *slot = Some(HartEntry::new(hart));
        Ok(())
    }

    pub(crate) fn submit(&mut self, unit: UnitId, affinity: Option<usize>) -> Result<(), Error> {
        if self.admitted(unit) {
            return Err(Error::Busy);
        }
        if affinity.is_some_and(|hart| self.participation(hart).is_none()) {
            return Err(Error::Unsupported);
        }
        // Reserve room for all admitted Units to return to Ready after Yield.
        let occupied = self
            .harts
            .iter()
            .flatten()
            .filter(|hart| hart.on_hart.is_some());
        if self.ready.iter().flatten().count() + occupied.count() >= CAPACITY {
            return Err(Error::Capacity);
        }
        self.enqueue(Ready {
            unit,
            affinity,
            sequence: 1,
        })
    }

    fn enqueue(&mut self, ready: Ready) -> Result<(), Error> {
        let tail = self
            .ready
            .iter_mut()
            .find(|slot| slot.is_none())
            .ok_or(Error::Capacity)?;
        *tail = Some(ready);
        Ok(())
    }

    pub(crate) fn admitted(&self, unit: UnitId) -> bool {
        self.ready.iter().flatten().any(|ready| ready.unit == unit)
            || self
                .harts
                .iter()
                .flatten()
                .filter_map(|hart| hart.on_hart.as_ref())
                .any(|state| state.occupancy.unit == unit)
    }

    fn participation(&self, hart: usize) -> Option<Participation> {
        self.harts
            .iter()
            .position(|slot| slot.as_ref().is_some_and(|attached| attached.id == hart))
            .map(Participation)
    }

    fn available(&self, hart: usize) -> Result<Participation, Error> {
        let participation = self.participation(hart).ok_or(Error::Unsupported)?;
        let participant = self.harts[participation.0].as_ref().expect("attached Hart");
        if participant.on_hart.is_some() {
            return Err(Error::Busy);
        }
        Ok(participation)
    }

    fn first_ready(&self, hart: usize) -> Option<(usize, Ready)> {
        self.ready.iter().enumerate().find_map(|(slot, ready)| {
            let ready = (*ready)?;
            ready
                .affinity
                .is_none_or(|target| target == hart)
                .then_some((slot, ready))
        })
    }

    pub(crate) fn candidate(&self, hart: usize) -> Result<Option<UnitId>, Error> {
        self.available(hart)?;
        Ok(self.first_ready(hart).map(|(_, ready)| ready.unit))
    }

    pub(crate) fn occupy(&mut self, hart: usize, unit: UnitId) -> Result<Occupancy, Error> {
        let participation = self.available(hart)?;
        let (slot, ready) = self
            .first_ready(hart)
            .filter(|(_, ready)| ready.unit == unit)
            .ok_or(Error::State)?;
        let occupancy = Occupancy {
            unit,
            hart,
            participation,
            sequence: ready.sequence,
        };
        self.ready[slot..].rotate_left(1);
        self.ready[CAPACITY - 1] = None;
        let participant = self.harts[participation.0].as_mut().expect("attached Hart");
        participant.on_hart = Some(OnHart {
            occupancy,
            affinity: ready.affinity,
            leave: None,
        });
        Ok(occupancy)
    }

    pub(crate) fn on_hart(&self, occupancy: Occupancy) -> Result<(), Error> {
        self.active(occupancy).map(|_| ())
    }

    fn active(&self, occupancy: Occupancy) -> Result<&OnHart, Error> {
        self.harts[occupancy.participation.0]
            .as_ref()
            .and_then(|hart| hart.on_hart.as_ref())
            .filter(|state| state.occupancy == occupancy)
            .ok_or(Error::Gone)
    }

    pub(crate) fn request_leave(
        &mut self,
        occupancy: Occupancy,
        destination: Destination,
    ) -> Result<(), Error> {
        let state = self.active(occupancy)?;
        if state.leave.is_some() {
            return Err(Error::Busy);
        }
        let leave = match destination {
            Destination::Ready => Leave::Ready(
                // A Unit without another occupancy identity can still return.
                occupancy.sequence.checked_add(1).ok_or(Error::Capacity)?,
            ),
            Destination::Release => Leave::Release,
        };
        let participant = self.harts[occupancy.participation.0]
            .as_mut()
            .expect("attached Hart");
        participant
            .on_hart
            .as_mut()
            .expect("validated occupancy")
            .leave = Some(leave);
        Ok(())
    }

    pub(crate) fn vacate(&mut self, receipt: LeaveReceipt) -> Result<(), Error> {
        let occupancy = receipt.occupancy();
        let state = self.active(occupancy)?;
        if let Leave::Ready(sequence) = state.leave.ok_or(Error::State)? {
            self.enqueue(Ready {
                unit: occupancy.unit,
                affinity: state.affinity,
                sequence,
            })?;
        }
        let participant = self.harts[occupancy.participation.0]
            .as_mut()
            .expect("attached Hart");
        participant.on_hart = None;
        Ok(())
    }
}
