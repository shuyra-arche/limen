//! Independently prepaid Return positions and retained settlement history.

use crate::metal::{
    identity::treaty::Key,
    sync::treaty::{TurnLock, TurnMode, TurnWorkspace},
};
use limen_treaty::model::PhysicalRange;

use super::treaty::ReturnReceipt;

use super::{
    Allocation, AllocationId, Allocator, AllowanceId, Error, RegionId,
    allocation::{Phase, remove},
    region::State,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ReturnScope(pub(super) RegionId);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReturnId {
    pub(super) key: Key<ReturnScope>,
}

impl ReturnId {
    fn region(self) -> RegionId {
        self.key.scope().0
    }
}

/// Ownership and completed retirement of the exact original material.
#[must_use]
pub struct Retired(pub(super) Allocation);

impl Allocation {
    /// The prepaid Return location belongs to the upstream source's table.
    /// Learning its identity does not authorize handoff or completion.
    pub(super) fn return_position(&self) -> Result<ReturnId, Error> {
        let region = self.source().region(self.id.region())?;
        region
            .state
            .acquire(|state| state.active(self).map(|record| record.retirement))?
    }

    /// Conclude the consumer's actual access lifecycle.
    ///
    /// # Safety
    ///
    /// The exact allocation must be closed to new use. Every applicable
    /// software access, CPU translation, device access and in-flight use must
    /// be retired. All proofs must remain valid through handoff. Owning this
    /// value, an address or a closed spending permission proves none of these.
    pub unsafe fn retire(self) -> Retired {
        Retired(self)
    }
}

impl Retired {
    pub fn physical(&self) -> PhysicalRange {
        self.0.physical
    }

    /// Hand off to the original source and record the prepaid refund
    /// receipt before releasing its Region Turn.
    ///
    /// # Safety
    ///
    /// The output must be the actual owner's durable receipt position.
    pub unsafe fn begin_return(
        self,
        receipt: &mut Option<ReturnReceipt>,
    ) -> Result<(), (Self, Error)> {
        if receipt.is_some() {
            return Err((self, Error::Receiver));
        }
        let allocation = &self.0;
        let source = allocation.source();
        let result = (|| {
            let region = source.region(allocation.id.region())?;
            region.state.acquire(|state| {
                // The original Retired input certifies all prior uses ended.
                let id = unsafe { state.begin_retired(allocation, ReceiptOwner::Receiver) }?;
                *receipt = Some(ReturnReceipt::new(source, id));
                Ok(())
            })?
        })();
        result.map_err(|error| (self, error))
    }
}

#[derive(Clone, Copy)]
pub(super) enum ReturnRecord {
    Reserved(AllocationId),
    Pending(AllocationId, AllowanceId, ReceiptOwner),
    Done(Returned, ReceiptOwner),
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum ReceiptOwner {
    Pool,
    Receiver,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Returned {
    pub allocation: AllocationId,
    pub physical: PhysicalRange,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Pending {
    Found(ReturnId),
    More,
    End,
}

impl Allocator {
    pub(super) fn return_state(&self, id: ReturnId) -> Result<&TurnLock<State>, Error> {
        Ok(&self.region(id.region())?.state)
    }

    /// Finish a prepaid return once. Done can be replayed independently of
    /// the active slot and the original Allowance.
    pub fn finish_return(&self, id: ReturnId, mode: TurnMode) -> Result<Returned, Error> {
        let region = self.region(id.region())?;
        let allowance = match region
            .state
            .acquire(|state| state.returns.get(id.key).copied())?
        {
            Some(ReturnRecord::Pending(_, allowance, _)) => allowance,
            Some(ReturnRecord::Done(done, _)) => return Ok(done),
            _ => return Err(Error::Stale),
        };
        let slot = self.slot(allowance)?;
        let mut work = TurnWorkspace::<2>::new();
        let mut plan = work.plan();
        plan.insert(&region.state).expect("return Region");
        plan.insert(slot).expect("original Allowance");
        plan.acquire(mode, |held| {
            let state = held.access(&region.state).expect("return Region");
            let (allocation, owner) = match state.returns.get(id.key) {
                Some(ReturnRecord::Pending(allocation, original, owner))
                    if *original == allowance =>
                {
                    (*allocation, *owner)
                }
                Some(ReturnRecord::Done(done, _)) => return Ok(*done),
                _ => return Err(Error::Stale),
            };
            let record = *state.allocations.get(allocation.key).ok_or(Error::Stale)?;
            if record.retirement != id
                || record.allowance != allowance
                || !matches!(record.phase, Phase::Returning)
            {
                return Err(Error::Stale);
            }

            let (_, open) = held
                .access(slot)
                .expect("original Allowance")
                .live(allowance)?;
            let amount = record.physical.length().get();
            let state = held.access(&region.state).expect("return Region");
            let credit = if open { 0 } else { amount };
            let unassigned = state
                .unassigned()
                .checked_add(credit)
                .ok_or(Error::Balance)?;
            state
                .buddy()
                .release(record.physical)
                .expect("original occupied range");
            state.set_unassigned(unassigned);
            remove(&mut state.allocations, allocation.key);
            let done = Returned {
                allocation,
                physical: record.physical,
            };
            *state.returns.get_mut(id.key).expect("original Return") =
                ReturnRecord::Done(done, owner);
            let refund = held
                .access(slot)
                .expect("original Allowance")
                .refund(amount, true);
            debug_assert_eq!(refund, credit);
            Ok(done)
        })?
    }
}

impl State {
    /// The caller holds the original Region and proves all mapped, software
    /// and hardware uses of this Allocation have ended.
    pub(super) unsafe fn begin_retired(
        &mut self,
        allocation: &Allocation,
        owner: ReceiptOwner,
    ) -> Result<ReturnId, Error> {
        let record = self.active(allocation)?;
        let retirement = self
            .returns
            .get_mut(record.retirement.key)
            .ok_or(Error::Stale)?;
        if !matches!(*retirement, ReturnRecord::Reserved(id) if id == allocation.id) {
            return Err(Error::Stale);
        }
        *retirement = ReturnRecord::Pending(allocation.id, record.allowance, owner);
        self.allocations
            .get_mut(allocation.id.key)
            .expect("active")
            .phase = Phase::Returning;
        Ok(record.retirement)
    }

    pub(super) fn acknowledge_return(
        &mut self,
        id: ReturnId,
        owner: ReceiptOwner,
    ) -> Result<(), Error> {
        match self.returns.get(id.key) {
            Some(ReturnRecord::Done(_, original)) if *original == owner => {}
            Some(ReturnRecord::Done(..)) => return Err(Error::Protection),
            _ => return Err(Error::Stale),
        }
        remove(&mut self.returns, id.key);
        Ok(())
    }
}
