//! An Allocator-issued limit carried by the current spending holding.

use core::ptr;
use limen_treaty::model::{Alignment, ByteLength};

use crate::metal::sync::{TurnAccess, TurnPlan};

use super::super::{Allocator, Allowance, AllowanceId, Candidate, Error, RamPurpose};

/// Immutable purpose of one permanently supplied initial Pend receiver.
/// Pend controls whether its single holding is installed or can still spend.
pub(crate) struct AllocationPermitSlot {
    purpose: RamPurpose,
}

impl AllocationPermitSlot {
    pub(crate) const fn metadata() -> Self {
        Self {
            purpose: RamPurpose::Metadata,
        }
    }

    pub(in crate::metal::memory::allocator) fn matches(&self, other: &Self) -> bool {
        ptr::eq(self, other)
    }
}

#[derive(Clone, Copy)]
pub(crate) struct LayoutLimit {
    maximum: ByteLength,
    alignment: Alignment,
}

impl LayoutLimit {
    pub(crate) fn new(maximum: ByteLength, alignment: Alignment) -> Result<Self, Error> {
        if maximum.get() == 0 {
            return Err(Error::PermitLimit);
        }
        Ok(Self { maximum, alignment })
    }
}

/// The exact share and policy of an initial spending holding. A copy of this
/// value identifies the holding and its limits but does not establish liveness.
#[derive(Clone, Copy)]
pub(crate) struct AllocationPermit {
    source: &'static Allocator,
    allowance: AllowanceId,
    slot: &'static AllocationPermitSlot,
    limit: LayoutLimit,
}

impl AllocationPermit {
    pub(crate) fn source(self) -> &'static Allocator {
        self.source
    }

    pub(crate) fn allowance(self) -> AllowanceId {
        self.allowance
    }

    pub(in crate::metal::memory::allocator) fn slot(self) -> &'static AllocationPermitSlot {
        self.slot
    }

    pub(in crate::metal::memory::allocator) fn check(self, source: &Allocator, candidate: Candidate) -> Result<(), Error> {
        if !ptr::eq(source, self.source) {
            return Err(Error::PermitOrigin);
        }
        let request = candidate.request();
        if request.purpose() != self.slot.purpose
            || request.length() > self.limit.maximum
            || request.alignment() > self.limit.alignment
        {
            return Err(Error::PermitLimit);
        }
        Ok(())
    }
}

impl Allowance {
    pub(crate) fn plan_initial_permit<'a>(&self, plan: &mut TurnPlan<'a, '_>) -> Result<(), Error> {
        plan.insert(self.source().slot(self.id())?)
            .map_err(|_| Error::Capacity)
    }

    /// Issue a source-local policy for a permanent receiver whose installation
    /// the trusted boot and Pend owners are jointly committing.
    pub(crate) fn issue_initial_permit(
        &self,
        held: &mut TurnAccess<'_>,
        slot: &'static AllocationPermitSlot,
        limit: LayoutLimit,
    ) -> Result<AllocationPermit, Error> {
        held.access(self.source().slot(self.id())?)
            .ok_or(Error::Protection)?
            .open(self.id())?;
        Ok(AllocationPermit {
            source: self.source(),
            allowance: self.id(),
            slot,
            limit,
        })
    }
}
