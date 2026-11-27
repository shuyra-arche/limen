//! Actual physical occupation, allocation identities and atomic conversion.

mod construction;
mod storage;

pub use construction::{AllocationConstruction, Cancellation};
pub use storage::MappedStorage;

use core::ptr;

use crate::metal::{
    identity::treaty::{GenerationTable, Key, Reservation as SlotReservation, ReserveError},
    sync::treaty::{TurnMode, TurnWorkspace},
};
use limen_treaty::model::PhysicalRange;

use super::{
    Allocator, AllowanceId, Error, RegionId, Retired, ReturnId,
    region::State,
    returns::{ReturnRecord, ReturnScope},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct AllocationScope(pub(super) RegionId);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AllocationId {
    pub(super) key: Key<AllocationScope>,
}

impl AllocationId {
    pub(super) fn region(self) -> RegionId {
        self.key.scope().0
    }
}

#[must_use]
pub struct Allocation {
    source: &'static Allocator,
    pub(super) id: AllocationId,
    pub(super) physical: PhysicalRange,
}

impl Allocation {
    pub(super) fn issued(
        source: &'static Allocator,
        id: AllocationId,
        physical: PhysicalRange,
    ) -> Self {
        Self {
            source,
            id,
            physical,
        }
    }

    pub fn id(&self) -> AllocationId {
        self.id
    }

    pub fn region(&self) -> RegionId {
        self.id.region()
    }

    pub fn physical(&self) -> PhysicalRange {
        self.physical
    }

    pub(super) fn source(&self) -> &'static Allocator {
        self.source
    }
}

#[derive(Clone, Copy)]
pub(super) enum Phase {
    Reserved(&'static AllocationConstruction),
    Issued,
    Returning,
}

#[derive(Clone, Copy)]
pub(super) struct AllocationRecord {
    pub(super) physical: PhysicalRange,
    pub(super) allowance: AllowanceId,
    pub(super) retirement: ReturnId,
    pub(super) phase: Phase,
}

impl State {
    pub(super) fn capacity(&self, count: usize) -> Result<(), Error> {
        let error = |reason, capacity| match reason {
            ReserveError::IdentityExhausted => Error::IdentityExhausted,
            _ => capacity,
        };
        self.allocations
            .check_reserve(count)
            .map_err(|e| error(e, Error::AllocationCapacity))?;
        self.returns
            .check_reserve(count)
            .map_err(|e| error(e, Error::ReturnCapacity))?;
        Ok(())
    }

    pub(super) fn record(
        &mut self,
        physical: PhysicalRange,
        allowance: AllowanceId,
        phase: Phase,
    ) -> AllocationId {
        // Every creation preflights both exact tables under this protection.
        let allocation = self
            .allocations
            .reserve()
            .expect("Allocation slot preflighted");
        let retirement = self.returns.reserve().expect("Return slot preflighted");
        self.publish_record(allocation, retirement, physical, allowance, phase)
    }

    fn publish_record(
        &mut self,
        allocation: SlotReservation<AllocationScope>,
        retirement: SlotReservation<ReturnScope>,
        physical: PhysicalRange,
        allowance: AllowanceId,
        phase: Phase,
    ) -> AllocationId {
        let id = AllocationId {
            key: allocation.key(),
        };
        let return_id = ReturnId {
            key: retirement.key(),
        };
        self.returns
            .publish(retirement, ReturnRecord::Reserved(id))
            .unwrap_or_else(|_| unreachable!("new Return reservation"));
        self.allocations
            .publish(
                allocation,
                AllocationRecord {
                    physical,
                    allowance,
                    retirement: return_id,
                    phase,
                },
            )
            .unwrap_or_else(|_| unreachable!("new Allocation reservation"));
        id
    }

    pub(super) fn active(&self, allocation: &Allocation) -> Result<AllocationRecord, Error> {
        let record = *self
            .allocations
            .get(allocation.id.key)
            .ok_or(Error::Stale)?;
        if record.physical != allocation.physical || !matches!(record.phase, Phase::Issued) {
            return Err(Error::Stale);
        }
        Ok(record)
    }

    pub(super) fn settle_reserved(&mut self, id: AllocationId, record: AllocationRecord) {
        remove(&mut self.returns, record.retirement.key);
        remove(&mut self.allocations, id.key);
    }
}

pub(super) fn remove<Scope: Copy + Eq, T: Copy>(
    table: &mut GenerationTable<'_, Scope, T>,
    key: Key<Scope>,
) {
    table.detach(key).expect("exact published identity");
    let (reclamation, _) = table.reclaim(key).expect("exact detached identity");
    table
        .release(reclamation)
        .unwrap_or_else(|_| unreachable!("exact reclamation"));
}

impl Allocator {
    pub(super) fn convert<const INPUTS: usize, const OUTPUTS: usize>(
        &'static self,
        inputs: [&Retired; INPUTS],
        outputs: [PhysicalRange; OUTPUTS],
        receivers: &mut [Option<Allocation>; OUTPUTS],
        mode: TurnMode,
    ) -> Result<(), Error> {
        if receivers.iter().any(Option::is_some) {
            return Err(Error::Receiver);
        }
        if inputs.iter().any(|input| !ptr::eq(self, input.0.source)) {
            return Err(Error::Stale);
        }
        let region = self.region(inputs[0].0.id.region())?;
        let allowance = region
            .state
            .acquire(|state| state.active(&inputs[0].0))??
            .allowance;
        let slot = self.slot(allowance)?;
        let mut work = TurnWorkspace::<2>::new();
        let mut plan = work.plan();
        plan.insert(&region.state).expect("conversion Region");
        plan.insert(slot).expect("original Allowance");
        plan.acquire(mode, |held| {
            held.access(slot).expect("Allowance").open(allowance)?;
            let state = held.access(&region.state).expect("Region");
            let mut records = [None; INPUTS];
            for (cell, input) in records.iter_mut().zip(inputs) {
                let record = state.active(&input.0)?;
                if record.allowance != allowance {
                    return Err(Error::Balance);
                }
                if !matches!(state.returns.get(record.retirement.key),
                    Some(ReturnRecord::Reserved(original)) if *original == input.0.id)
                {
                    return Err(Error::Stale);
                }
                *cell = Some(record);
            }
            state.capacity(OUTPUTS)?;
            let mut allocations = [const { None }; OUTPUTS];
            let mut returns = [const { None }; OUTPUTS];
            state
                .allocations
                .reserve_many(&mut allocations)
                .expect("preflighted output slots");
            state
                .returns
                .reserve_many(&mut returns)
                .expect("preflighted output returns");

            for ((physical, receiver), (allocation, retirement)) in outputs
                .into_iter()
                .zip(receivers)
                .zip(allocations.into_iter().zip(returns))
            {
                let allocation = allocation.expect("output allocation reservation");
                let retirement = retirement.expect("output return reservation");
                let allocation_id = state.publish_record(
                    allocation,
                    retirement,
                    physical,
                    allowance,
                    Phase::Issued,
                );
                *receiver = Some(Allocation::issued(self, allocation_id, physical));
            }
            for (input, record) in inputs.into_iter().zip(records) {
                state.settle_reserved(input.0.id, record.expect("all inputs preflighted"));
            }
            Ok(())
        })?
    }
}
