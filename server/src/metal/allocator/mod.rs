//! Fixed physical sources and their local allocation and settlement records.

use core::sync::atomic::{AtomicUsize, Ordering::Acquire};

use crate::{
    address::PhysicalRange,
    entry::boot::Region as Description,
    sync::{Acquisition, AcquisitionError, TurnLock, Work},
};

mod allowance;
pub mod buddy;
pub(crate) mod bump;
pub mod pool;
mod region;
mod search;
mod storage;
mod transfer;

pub use allowance::{Allowance, AllowanceId, Balance};
pub use region::Grant;
pub use search::{Candidate, Lookup, RamPurpose, Request, Search};
pub use storage::{Config, Initial, MappedStorage};
pub(crate) use transfer::Spender;
pub use transfer::{
    Allocation, AllocationId, Construction, Pending, Retired, ReturnId, ReturnReceipt, Returned,
};

use allowance::{Free, Slot};
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
    Protection,
    Busy,
    Stale,
    Shut,
    Balance,
    Changed,
}

impl From<AcquisitionError> for Error {
    fn from(error: AcquisitionError) -> Self {
        match error {
            AcquisitionError::Busy => Self::Busy,
            AcquisitionError::Capacity => Self::Capacity,
            AcquisitionError::Empty | AcquisitionError::Context => Self::Protection,
        }
    }
}

/// Maximum Region count, including delayed admission positions. Exact source
/// inspection needs this many nodes plus the directory node.
pub const REGION_LIMIT: usize = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RegionId(usize);

/// A fixed source. Its root and canonical locks live for the runtime's lifetime.
pub struct Allocator {
    config: Config,
    root: PhysicalRange,
    directory: TurnLock<()>,
    published: AtomicUsize,
    regions: &'static [Region],
    allowances: &'static [TurnLock<Slot>],
    free: TurnLock<Free>,
}

impl Allocator {
    pub fn root(&self) -> PhysicalRange {
        self.root
    }

    pub fn regions(&self) -> impl Iterator<Item = (RegionId, Description)> + '_ {
        (0..self.published.load(Acquire)).map(|index| {
            let id = RegionId(index);
            (id, self.regions[index].description())
        })
    }

    fn region(&self, id: RegionId) -> Result<&Region, Error> {
        if id.0 >= self.published.load(Acquire) {
            return Err(Error::Stale);
        }
        Ok(&self.regions[id.0])
    }
}

pub(crate) fn with_turn<T, R>(
    lock: &TurnLock<T>,
    operation: impl FnOnce(&mut T) -> R,
) -> Result<R, Error> {
    let mut work = Work::<1>::new();
    let mut plan = work.plan();
    plan.insert(lock).expect("one Turn");
    Ok(plan.execute(Acquisition::Fast, |held| {
        operation(held.access(lock).expect("planned lock"))
    })?)
}
