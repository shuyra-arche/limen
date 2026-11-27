//! Observe and prepare against the exact funded source.

use crate::metal::sync::treaty::TurnPlan;
use limen_treaty::model::ByteLength;

use super::{Allocator, Allowance, AllowanceCandidate, AllowanceId, Balance, Error, RegionId};

impl Allowance {
    pub fn source(&self) -> &'static Allocator {
        self.source
    }

    pub fn id(&self) -> AllowanceId {
        self.id
    }

    pub fn balance(&self) -> Result<Balance, Error> {
        self.source
            .slot(self.id)?
            .acquire(|slot| slot.live(self.id).map(|pair| pair.0))?
    }
}

impl Allocator {
    /// Observe the free-chain head before constructing the complete plan.
    pub fn allowance_candidate(&self) -> Result<AllowanceCandidate, Error> {
        let slot = self.free.acquire(|free| {
            free.head.ok_or(if free.retired == self.allowances.len() {
                Error::IdentityExhausted
            } else {
                Error::AllowanceCapacity
            })
        })??;
        Ok(AllowanceCandidate {
            source: self.source_id(),
            slot,
        })
    }

    pub fn plan_allowance<'a>(
        &'a self,
        plan: &mut TurnPlan<'a, '_>,
        candidate: AllowanceCandidate,
        debits: &[(RegionId, ByteLength)],
    ) -> Result<(), Error> {
        if debits.len() > self.config.regions {
            return Err(Error::Capacity);
        }
        let slot = self.allowance_slot(candidate.source, candidate.slot)?;
        plan.insert(&self.free).map_err(|_| Error::Capacity)?;
        plan.insert(slot).map_err(|_| Error::Capacity)?;
        for &(region, _) in debits {
            plan.insert(&self.region(region)?.state)
                .map_err(|_| Error::Capacity)?;
        }
        Ok(())
    }

    pub fn plan_transfer<'a>(
        &'a self,
        plan: &mut TurnPlan<'a, '_>,
        from: AllowanceId,
        to: AllowanceId,
    ) -> Result<(), Error> {
        plan.insert(self.slot(from)?).map_err(|_| Error::Capacity)?;
        plan.insert(self.slot(to)?).map_err(|_| Error::Capacity)?;
        Ok(())
    }
}
