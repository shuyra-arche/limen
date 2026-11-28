//! Fixed local ordinary windows and their original Charge accounts.

use core::{num::NonZeroUsize, time::Duration};

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

/// One ordinary window in the original resident Fund. Its ordinal is never
/// reused in that Fund, even after its account has closed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PeriodKey {
    fund: usize,
    window: NonZeroUsize,
}

impl PeriodKey {
    pub(in crate::basis) fn belongs_to(self, fund: usize) -> bool {
        self.fund == fund
    }

    pub const fn window(self) -> NonZeroUsize {
        self.window
    }
}

/// The final original account. Retained in its supplied record after advance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PeriodSettlement {
    pub period: PeriodKey,
    pub quota: Duration,
    pub spent: Duration,
    pub free: Duration,
    pub excess: Duration,
}

/// The closed original charge and the resulting balance of its own Period.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChargeSettlement {
    pub period: PeriodKey,
    pub occupancy: Occupancy,
    pub evidence: DurationBounds,
    pub spent: Duration,
    pub free: Duration,
    pub excess: Duration,
}

struct Period {
    key: PeriodKey,
    quota: Duration,
    free: Duration,
    spent: Duration,
    excess: Duration,
    charge: Option<Charge>,
}

impl Period {
    fn new(key: PeriodKey, quota: Duration) -> Self {
        Self {
            key,
            quota,
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
            period: self.key,
            occupancy,
            evidence,
            spent: self.spent.checked_add(billed).ok_or(Error::Capacity)?,
            free: self.free.checked_add(refund).ok_or(Error::Capacity)?,
            excess: self.excess.checked_add(overspend).ok_or(Error::Capacity)?,
        })
    }

    fn close(&self) -> Result<PeriodSettlement, Error> {
        if self.charge.is_some() {
            return Err(Error::Busy);
        }
        Ok(PeriodSettlement {
            period: self.key,
            quota: self.quota,
            spent: self.spent,
            free: self.free,
            excess: self.excess,
        })
    }

    fn settle(&mut self, result: ChargeSettlement) {
        debug_assert_eq!(self.key, result.period);
        self.free = result.free;
        self.spent = result.spent;
        self.excess = result.excess;
        self.charge = None;
    }
}

/// One supplied record per certified ordinary window. Old accounts remain
/// in their original slots and are never reused by another window.
pub struct PeriodRecord {
    period: Option<Period>,
}

impl PeriodRecord {
    pub const EMPTY: Self = Self { period: None };

    pub(in crate::basis) fn accepts(records: &[Self]) -> bool {
        !records.is_empty() && records.iter().all(|record| record.period.is_none())
    }
}

pub(super) struct Periods {
    records: &'static mut [PeriodRecord],
    current: usize,
}

impl Periods {
    fn new(fund: usize, quota: Duration, records: &'static mut [PeriodRecord]) -> Self {
        let key = PeriodKey {
            fund,
            window: NonZeroUsize::MIN,
        };
        records[0].period = Some(Period::new(key, quota));
        Self {
            records,
            current: 0,
        }
    }

    fn current(&self) -> Result<&Period, Error> {
        self.records
            .get(self.current)
            .and_then(|record| record.period.as_ref())
            .ok_or(Error::Gone)
    }

    fn current_mut(&mut self) -> &mut Period {
        self.records[self.current]
            .period
            .as_mut()
            .expect("current Period")
    }

    fn closed(&self, key: PeriodKey) -> Result<PeriodSettlement, Error> {
        let index = key.window.get() - 1;
        if index >= self.current {
            return Err(Error::Gone);
        }
        let period = self.records[index].period.as_ref().ok_or(Error::Gone)?;
        if period.key != key {
            return Err(Error::Gone);
        }
        period.close()
    }

    fn advance(&mut self, expected: PeriodKey) -> Result<PeriodSettlement, Error> {
        let period = self.current()?;
        if period.key != expected {
            return Err(Error::Gone);
        }
        let closed = period.close()?;
        let next = self.current.checked_add(1).ok_or(Error::Capacity)?;
        if next == self.records.len() {
            // The finite certified schedule has ended; retain the last account.
            self.current = next;
            return Ok(closed);
        }
        if closed.excess != Duration::ZERO {
            // A later window does not re-certify an invalid supply.
            return Err(Error::Budget);
        }
        let window = expected.window.checked_add(1).ok_or(Error::Capacity)?;
        let record = self.records.get_mut(next).ok_or(Error::Capacity)?;
        if record.period.is_some() {
            return Err(Error::State);
        }
        let key = PeriodKey {
            fund: expected.fund,
            window,
        };
        record.period = Some(Period::new(key, closed.quota));
        self.current = next;
        Ok(closed)
    }
}

impl<const CAPACITY: usize> Room<CAPACITY> {
    pub(in crate::basis) fn attach_fund(
        &mut self,
        processor: ProcessorId,
        fund: usize,
        quota: Duration,
        records: &'static mut [PeriodRecord],
    ) -> Result<(), (Error, &'static mut [PeriodRecord])> {
        if let Err(error) = self.attach(processor) {
            return Err((error, records));
        }
        let index = self.participation(processor).expect("attached Fund");
        let participant = self.participants[index.0].as_mut().expect("participant");
        participant.queue = Queue::Funded(fund);
        participant.periods = Some(Periods::new(fund, quota, records));
        Ok(())
    }

    pub(in crate::basis) fn funded_period(
        &self,
        processor: ProcessorId,
        fund: usize,
    ) -> Result<PeriodKey, Error> {
        let index = self.available(processor, Queue::Funded(fund))?;
        Ok(self.periods(index).current()?.key)
    }

    pub(in crate::basis) fn closed_period(
        &self,
        processor: ProcessorId,
        fund: usize,
        key: PeriodKey,
    ) -> Result<PeriodSettlement, Error> {
        let index = self.available(processor, Queue::Funded(fund))?;
        self.periods(index).closed(key)
    }

    pub(in crate::basis) fn advance_period(
        &mut self,
        processor: ProcessorId,
        fund: usize,
        expected: PeriodKey,
    ) -> Result<PeriodSettlement, Error> {
        let index = self.available(processor, Queue::Funded(fund))?;
        self.periods_mut(index).advance(expected)
    }

    pub(in crate::basis) fn funded_candidate(
        &self,
        processor: ProcessorId,
        fund: usize,
        ceiling: Duration,
    ) -> Result<Option<(UnitId, PeriodKey)>, Error> {
        let queue = Queue::Funded(fund);
        let Some(unit) = self.candidate_for(processor, queue)? else {
            return Ok(None);
        };
        let index = self.available(processor, queue)?;
        let period = self.periods(index).current()?;
        period.reserve(ceiling)?;
        Ok(Some((unit, period.key)))
    }

    pub(in crate::basis) fn occupy_funded(
        &mut self,
        processor: ProcessorId,
        fund: usize,
        unit: UnitId,
        expected: PeriodKey,
        ceiling: Duration,
    ) -> Result<Occupancy, Error> {
        let queue = Queue::Funded(fund);
        let index = self.available(processor, queue)?;
        let period = self.periods(index).current()?;
        if period.key != expected {
            return Err(Error::Gone);
        }
        let free = period.reserve(ceiling)?;
        let occupancy = self.occupy_for(processor, unit, queue)?;
        let period = self.periods_mut(index).current_mut();
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
        let settlement = self
            .periods(index)
            .current()?
            .settlement(occupancy, meter)?;
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
        self.periods_mut(index).current_mut().settle(settlement);
        Ok(settlement)
    }

    pub(in crate::basis) fn check_unfunded(&self, occupancy: Occupancy) -> Result<(), Error> {
        self.check_occupied(occupancy)?;
        let participant = self.participants[occupancy.participation.0]
            .as_ref()
            .expect("participant");
        if participant.periods.is_some() {
            return Err(Error::Unsupported);
        }
        Ok(())
    }

    fn periods(&self, id: super::ParticipationId) -> &Periods {
        self.participants[id.0]
            .as_ref()
            .and_then(|p| p.periods.as_ref())
            .expect("funded participant")
    }

    fn periods_mut(&mut self, id: super::ParticipationId) -> &mut Periods {
        self.participants[id.0]
            .as_mut()
            .and_then(|p| p.periods.as_mut())
            .expect("funded participant")
    }
}
