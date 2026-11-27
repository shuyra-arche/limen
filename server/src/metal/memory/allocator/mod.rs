//! Fixed physical sources and their local allocation and settlement records.

use core::{
    ptr,
    sync::atomic::{AtomicUsize, Ordering::Acquire},
};

use crate::metal::{
    identity::Slot,
    sync::{TurnAcquireError, TurnLock},
};
use limen_treaty::model::PhysicalRange;

mod allocation;
mod allowance;
pub(crate) mod bootstrap;
pub mod pool;
pub mod region;
mod returns;
mod search;

pub use allocation::{Allocation, AllocationConstruction, AllocationId, MappedStorage};
pub(crate) use allowance::permit::{AllocationPermit, AllocationPermitSlot, LayoutLimit};
pub use allowance::{Allowance, AllowanceCandidate, AllowanceId, Balance, Budget, BudgetObservation, BudgetProgress};
pub(crate) use bootstrap::RootInitialization;
pub use bootstrap::{Config, Initial, RootStorage};
pub use region::{Grant, RegionDescription, RegionOrigin};
pub use returns::{Pending, Retired, ReturnId, ReturnReceipt, Returned};
pub use search::{Candidate, Lookup, LookupObservation, RamPurpose, Request, Search};

use allowance::{AllowanceSlot, Free};
use region::Region;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Layout,
    Range,
    Overlap,
    Capacity,
    IdentityExhausted,
    AllowanceCapacity,
    AllocationCapacity,
    ReturnCapacity,
    Receiver,
    PermitOrigin,
    PermitLimit,
    Protection,
    Busy,
    Stale,
    Shut,
    Balance,
    Changed,
}

impl From<TurnAcquireError> for Error {
    fn from(error: TurnAcquireError) -> Self {
        match error {
            TurnAcquireError::Busy => Self::Busy,
            TurnAcquireError::Capacity => Self::Capacity,
            TurnAcquireError::Empty | TurnAcquireError::Context => Self::Protection,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SourceId(usize);

/// A permanent position within the exact Allocator that admitted it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RegionId {
    source: SourceId,
    slot: Slot,
}

impl RegionId {
    fn new(source: SourceId, index: usize) -> Self {
        Self {
            source,
            slot: Slot::new(index).expect("checked Region capacity"),
        }
    }

    fn index(self) -> usize {
        self.slot.index()
    }
}

/// A fixed source. Its root and canonical locks live for the runtime's lifetime.
pub struct Allocator {
    config: Config,
    root: PhysicalRange,
    directory: TurnLock<()>,
    published: AtomicUsize,
    regions: &'static [Region],
    allowances: &'static [TurnLock<AllowanceSlot>],
    free: TurnLock<Free>,
}

impl Allocator {
    pub fn root(&self) -> PhysicalRange {
        self.root
    }

    pub fn regions(&self) -> impl Iterator<Item = (RegionId, RegionDescription)> + '_ {
        let source = self.source_id();
        (0..self.published.load(Acquire)).map(move |index| {
            let id = RegionId::new(source, index);
            (id, self.regions[index].description())
        })
    }

    fn source_id(&self) -> SourceId {
        // A published Allocator remains at the same root address for its
        // whole lifetime. The identity is used only for comparison.
        SourceId(ptr::from_ref(self).addr())
    }

    fn region(&self, id: RegionId) -> Result<&Region, Error> {
        if id.source != self.source_id() || id.index() >= self.published.load(Acquire) {
            return Err(Error::Stale);
        }
        Ok(&self.regions[id.index()])
    }
}

#[cfg(test)]
#[path = "../../../../tests/metal/memory/allocator/mod.rs"]
mod tests;
