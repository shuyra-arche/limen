//! Source-budget observation and movement of actual unassigned balances.

use core::sync::atomic::Ordering::Acquire;
use limen_treaty::model::ByteLength;

use crate::metal::sync::{TurnAccess, TurnPlan};
use super::super::{Allocator, Error, RegionId, region::Revision};

/// Actual storage for one Region's independent budget revision.
#[derive(Clone, Copy)]
pub struct BudgetObservation(Revision);

impl BudgetObservation {
    pub const EMPTY: Self = Self(Revision::Exhausted);
}

enum Pass {
    Observe,
    Verify,
    Directory,
    Finished,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BudgetProgress {
    Pending,
    Complete(ByteLength, ByteLength),
}

/// One coherent source observation. Each step obtains at most one local lock.
#[must_use]
pub struct Budget<'a> {
    source: &'a Allocator,
    observations: &'a mut [BudgetObservation],
    index: usize,
    pass: Pass,
    managed: ByteLength,
    unassigned: ByteLength,
}

impl Allocator {
    /// Begin an exact managed/unassigned observation, using caller storage.
    pub fn budget<'a>(
        &'a self,
        observations: &'a mut [BudgetObservation],
    ) -> Result<Budget<'a>, Error> {
        let count = self.directory.acquire(|_| self.published.load(Acquire))?;
        let observations = observations.get_mut(..count).ok_or(Error::Capacity)?;
        observations.fill(BudgetObservation::EMPTY);
        Ok(Budget {
            source: self,
            observations,
            index: 0,
            pass: Pass::Observe,
            managed: ByteLength::new(0),
            unassigned: ByteLength::new(0),
        })
    }

    /// Include the exact two existing Region ledgers in a source-owner move.
    pub fn plan_budget_transfer<'a>(
        &'a self,
        plan: &mut TurnPlan<'a, '_>,
        from: RegionId,
        to: RegionId,
    ) -> Result<(), Error> {
        if from == to {
            return Err(Error::Layout);
        }
        let from = self.region(from)?;
        let to = self.region(to)?;
        plan.insert(&from.state).map_err(|_| Error::Capacity)?;
        plan.insert(&to.state).map_err(|_| Error::Capacity)?;
        Ok(())
    }

    /// Move unassigned credit within this source, not material or a share.
    ///
    /// # Safety
    /// The caller holds and revalidates the actual source management authority
    /// under the complete plan. A move neither reserves nor grants an Allowance.
    pub unsafe fn transfer_budget(
        &self,
        held: &mut TurnAccess<'_>,
        from: RegionId,
        to: RegionId,
        amount: ByteLength,
    ) -> Result<(), Error> {
        let amount = amount.get();
        if from == to || !amount.is_multiple_of(self.config.granule.get()) {
            return Err(Error::Layout);
        }
        let from = self.region(from)?;
        let to = self.region(to)?;
        let (debit, credit) = held
            .access_pair(&from.state, &to.state)
            .ok_or(Error::Protection)?;
        let left = debit.unassigned().checked_sub(amount).ok_or(Error::Balance)?;
        let right = credit.unassigned().checked_add(amount).ok_or(Error::Balance)?;
        debit.set_unassigned(left);
        credit.set_unassigned(right);
        Ok(())
    }
}

impl Budget<'_> {
    pub fn step(&mut self) -> Result<BudgetProgress, Error> {
        let result = self.advance();
        if result.is_err() {
            self.pass = Pass::Finished;
        }
        result
    }

    fn advance(&mut self) -> Result<BudgetProgress, Error> {
        let count = self.observations.len();
        match self.pass {
            Pass::Observe if self.index == count => {
                self.index = 0;
                self.pass = Pass::Verify;
            }
            Pass::Observe => {
                let region = &self.source.regions[self.index];
                let (amount, revision) = region.state.acquire(|state| state.budget())?;
                if !revision.preserves(revision) {
                    return Err(Error::Changed);
                }
                self.managed = self.managed
                    .checked_add(region.description().physical.length())
                    .ok_or(Error::Balance)?;
                self.unassigned = self.unassigned
                    .checked_add(ByteLength::new(amount))
                    .ok_or(Error::Balance)?;
                self.observations[self.index] = BudgetObservation(revision);
                self.index += 1;
            }
            Pass::Verify if self.index == count => {
                self.pass = Pass::Directory;
            }
            Pass::Verify => {
                let current = self.source.regions[self.index]
                    .state.acquire(|state| state.budget().1)?;
                if !current.preserves(self.observations[self.index].0) {
                    return Err(Error::Changed);
                }
                self.index += 1;
            }
            Pass::Directory => {
                let count_now = self.source.directory.acquire(|_| self.source.published.load(Acquire))?;
                if count_now != count {
                    return Err(Error::Changed);
                }
                self.pass = Pass::Finished;
                return Ok(BudgetProgress::Complete(self.managed, self.unassigned));
            }
            Pass::Finished => return Err(Error::Stale),
        }
        Ok(BudgetProgress::Pending)
    }
}
