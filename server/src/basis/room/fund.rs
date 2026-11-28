//! The original local Period and its one in-flight Charge.

use core::time::Duration;

use super::{Error, LeaveDestination, LeaveRequest, Occupancy, Placement, Queue, Room, UnitId};
use crate::metal::{
    chrono::treaty::{DurationBounds, MeterReceipt},
    processor::treaty::{LeaveReceipt, ProcessorId},
};

#[derive(Clone, Copy)]
struct Charge {
    occupancy: Occupancy,
    ceiling: Duration,
}

/// The closed original charge and the resulting balance of its own Period.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChargeSettlement {
    pub occupancy: Occupancy,
    pub evidence: DurationBounds,
    pub spent: Duration,
    pub free: Duration,
    pub excess: Duration,
}

pub(in crate::basis) struct Period {
    free: Duration,
    spent: Duration,
    excess: Duration,
    charge: Option<Charge>,
}

impl Period {
    pub(in crate::basis) fn new(quota: Duration) -> Self {
        Self {
            free: quota,
            spent: Duration::ZERO,
            excess: Duration::ZERO,
            charge: None,
        }
    }

    fn reserve(&self, ceiling: Duration) -> Result<Duration, Error> {
        if self.charge.is_some() {
            return Err(Error::Busy);
        }
        if self.excess != Duration::ZERO {
            return Err(Error::Budget);
        }
        self.free.checked_sub(ceiling).ok_or(Error::Budget)
    }

    fn settlement(
        &self,
        occupancy: Occupancy,
        meter: MeterReceipt<Occupancy>,
    ) -> Result<ChargeSettlement, Error> {
        let charge = self
            .charge
            .filter(|charge| charge.occupancy == occupancy && meter.occupancy() == occupancy)
            .ok_or(Error::Gone)?;
        let evidence = meter.evidence().ok_or(Error::Timing)?;
        let billed = evidence.maximum;
        let refund = charge.ceiling.saturating_sub(billed);
        let overspend = billed.saturating_sub(charge.ceiling);
        Ok(ChargeSettlement {
            occupancy,
            evidence,
            spent: self.spent.checked_add(billed).ok_or(Error::Capacity)?,
            free: self.free.checked_add(refund).ok_or(Error::Capacity)?,
            excess: self.excess.checked_add(overspend).ok_or(Error::Capacity)?,
        })
    }

    fn settle(&mut self, result: ChargeSettlement) {
        self.free = result.free;
        self.spent = result.spent;
        self.excess = result.excess;
        self.charge = None;
    }
}

impl<const CAPACITY: usize> Room<CAPACITY> {
    pub(in crate::basis) fn attach_fund(
        &mut self,
        processor: ProcessorId,
        fund: usize,
        period: Period,
    ) -> Result<(), Error> {
        self.attach(processor)?;
        let index = self.participation(processor).expect("attached Fund");
        let participant = self.participants[index.0].as_mut().expect("participant");
        participant.queue = Queue::Funded(fund);
        participant.period = Some(period);
        Ok(())
    }

    pub(in crate::basis) fn funded_candidate(
        &self,
        processor: ProcessorId,
        fund: usize,
        ceiling: Duration,
    ) -> Result<Option<UnitId>, Error> {
        let queue = Queue::Funded(fund);
        let Some(unit) = self.candidate_for(processor, queue)? else {
            return Ok(None);
        };
        let index = self.available(processor, queue)?;
        self.period(index).reserve(ceiling)?;
        Ok(Some(unit))
    }

    pub(in crate::basis) fn occupy_funded(
        &mut self,
        processor: ProcessorId,
        fund: usize,
        unit: UnitId,
        ceiling: Duration,
    ) -> Result<Occupancy, Error> {
        let queue = Queue::Funded(fund);
        let index = self.available(processor, queue)?;
        let free = self.period(index).reserve(ceiling)?;
        let occupancy = self.occupy_for(processor, unit, queue)?;
        let period = self.period_mut(index);
        period.free = free;
        period.charge = Some(Charge { occupancy, ceiling });
        Ok(occupancy)
    }

    /// A new destination is supplied for a stop or terminal return.
    /// None consumes only an already published Wait and a cooperative Leave.
    pub(in crate::basis) fn settle_funded(
        &mut self,
        fund: usize,
        leave: LeaveReceipt<Occupancy>,
        meter: MeterReceipt<Occupancy>,
        destination: Option<LeaveDestination>,
    ) -> Result<ChargeSettlement, Error> {
        let occupancy = leave.occupancy();
        let index = occupancy.participation;
        let slot = self.active(occupancy)?;
        if self.participants[index.0]
            .as_ref()
            .expect("participant")
            .queue
            != Queue::Funded(fund)
        {
            return Err(Error::Gone);
        }
        let settlement = self.period(index).settlement(occupancy, meter)?;
        if let Some(destination) = destination {
            self.request_leave(occupancy, destination)?;
        } else {
            if leave.suspended() || leave.unused() || leave.discarded() {
                return Err(Error::State);
            }
            let entry = self.units[slot].as_ref().expect("occupied Unit");
            if !matches!(
                entry.placement,
                Placement::Occupied {
                    leave: Some(LeaveRequest::Wait { .. }),
                    ..
                }
            ) {
                return Err(Error::State);
            }
        }
        self.vacate(leave).expect("exact funded departure");
        self.period_mut(index).settle(settlement);
        Ok(settlement)
    }

    pub(in crate::basis) fn check_unfunded(&self, occupancy: Occupancy) -> Result<(), Error> {
        self.check_occupied(occupancy)?;
        let participant = self.participants[occupancy.participation.0]
            .as_ref()
            .expect("participant");
        if participant.period.is_some() {
            return Err(Error::Unsupported);
        }
        Ok(())
    }

    fn period(&self, id: super::ParticipationId) -> &Period {
        self.participants[id.0]
            .as_ref()
            .and_then(|p| p.period.as_ref())
            .expect("funded participant")
    }

    fn period_mut(&mut self, id: super::ParticipationId) -> &mut Period {
        self.participants[id.0]
            .as_mut()
            .and_then(|p| p.period.as_mut())
            .expect("funded participant")
    }
}
