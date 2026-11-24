//! Single-Hart admission and exact execution occupancy.

use super::unit::{CAPACITY, Error, UnitId};
use crate::metal::riscv::execution::LeaveReceipt;

/// One dispatch on the Boot Hart. The Unit generation also identifies its
/// admission because this Room admits a Unit once, until it ends.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Occupancy {
    unit: UnitId,
    hart: usize,
    sequence: u64,
}

impl Occupancy {
    pub fn unit(self) -> UnitId {
        self.unit
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
    sequence: u64,
}

struct OnHart {
    occupancy: Occupancy,
    leave: Option<Destination>,
}

pub(crate) struct Room {
    hart: usize,
    // The Ready list and OnHart slot are the only admitted positions.
    ready: [Option<Ready>; CAPACITY],
    on_hart: Option<OnHart>,
}

impl Room {
    pub(crate) const fn new(hart: usize) -> Self {
        Self {
            hart,
            ready: [None; CAPACITY],
            on_hart: None,
        }
    }

    pub(crate) fn submit(&mut self, unit: UnitId) -> Result<(), Error> {
        if self.admitted(unit) {
            return Err(Error::Busy);
        }
        self.enqueue(Ready { unit, sequence: 1 })
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
        self.on_hart
            .as_ref()
            .is_some_and(|state| state.occupancy.unit == unit)
            || self.ready.iter().flatten().any(|ready| ready.unit == unit)
    }

    pub(crate) fn candidate(&self) -> Result<Option<UnitId>, Error> {
        if self.on_hart.is_some() {
            return Err(Error::Busy);
        }
        Ok(self.ready[0].map(|ready| ready.unit))
    }

    pub(crate) fn occupy(&mut self, unit: UnitId) -> Result<Occupancy, Error> {
        if self.candidate()? != Some(unit) {
            return Err(Error::State);
        }
        let ready = self.ready[0].expect("validated ready Unit");
        let occupancy = Occupancy {
            unit,
            hart: self.hart,
            sequence: ready.sequence,
        };
        self.ready.rotate_left(1);
        self.ready[CAPACITY - 1] = None;
        self.on_hart = Some(OnHart {
            occupancy,
            leave: None,
        });
        Ok(occupancy)
    }

    pub(crate) fn on_hart(&self, occupancy: Occupancy) -> Result<(), Error> {
        match &self.on_hart {
            Some(state) if state.occupancy == occupancy => Ok(()),
            _ => Err(Error::Gone),
        }
    }

    pub(crate) fn request_leave(
        &mut self,
        occupancy: Occupancy,
        destination: Destination,
    ) -> Result<(), Error> {
        self.on_hart(occupancy)?;
        let state = self.on_hart.as_mut().unwrap();
        if state.leave.is_some() {
            return Err(Error::Busy);
        }
        if matches!(destination, Destination::Ready) && occupancy.sequence == u64::MAX {
            // A Unit without another occupancy identity can still return.
            return Err(Error::Capacity);
        }
        state.leave = Some(destination);
        Ok(())
    }

    pub(crate) fn vacate(&mut self, receipt: LeaveReceipt) -> Result<(), Error> {
        let occupancy = receipt.occupancy();
        self.on_hart(occupancy)?;
        match self.on_hart.as_ref().unwrap().leave {
            Some(Destination::Ready) => self.enqueue(Ready {
                unit: occupancy.unit,
                sequence: occupancy.sequence.checked_add(1).ok_or(Error::Capacity)?,
            })?,
            Some(Destination::Release) => {}
            None => return Err(Error::State),
        }
        self.on_hart = None;
        Ok(())
    }
}
