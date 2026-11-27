//! Exact retained refund receipts and recovery observations.

use crate::metal::sync::treaty::TurnMode;

use super::super::returns::{ReceiptOwner, ReturnRecord};
use super::{Allocator, Error, Pending, RegionId, ReturnId, Returned};

/// The original owner's prepaid refund and acknowledgement position.
#[must_use]
pub struct ReturnReceipt {
    source: &'static Allocator,
    id: ReturnId,
}

impl ReturnReceipt {
    pub(in crate::metal::memory::allocator) const fn new(
        source: &'static Allocator,
        id: ReturnId,
    ) -> Self {
        Self { source, id }
    }

    /// Locate the original return for recovery, without acknowledgement authority.
    pub fn id(&self) -> ReturnId {
        self.id
    }

    /// Finish the original refund or read its retained result.
    pub fn finish(&self, mode: TurnMode) -> Result<Returned, Error> {
        self.source.finish_return(self.id, mode)
    }

    /// Release the original owner's completed receipt position.
    pub fn acknowledge(&self) -> Result<(), Error> {
        self.source
            .return_state(self.id)?
            .acquire(|state| state.acknowledge_return(self.id, ReceiptOwner::Receiver))?
    }
}

// Independent per-call work limit under the original Region's Turn.
const RETURN_SCAN_LIMIT: usize = 64;

impl Allocator {
    /// Advance a caller-owned cyclic recovery pass through the prepaid table,
    /// inspecting at most the caller's budget capped at 64 entries.
    /// A failed acquisition leaves the cursor unchanged. End resets to zero,
    /// so subsequent passes revisit Pending entries and delayed publications.
    pub fn pending(
        &self,
        id: RegionId,
        cursor: &mut usize,
        budget: usize,
    ) -> Result<Pending, Error> {
        let region = self.region(id)?;
        Ok(region.state.acquire(|state| {
            let end = state.returns.capacity();
            let stop = (*cursor)
                .saturating_add(budget.min(RETURN_SCAN_LIMIT))
                .min(end);
            while *cursor < stop {
                let current = *cursor;
                *cursor += 1;
                if let Some((key, ReturnRecord::Pending(..))) = state.returns.published_at(current)
                {
                    return Pending::Found(ReturnId { key });
                }
            }
            if *cursor >= end {
                *cursor = 0;
                Pending::End
            } else {
                Pending::More
            }
        })?)
    }
}
