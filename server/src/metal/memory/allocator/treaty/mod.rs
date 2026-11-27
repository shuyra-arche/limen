//! Ordinary physical material and typed storage use.

use crate::metal::sync::treaty::TurnAcquireError;
use core::sync::atomic::Ordering::Acquire;
use limen_treaty::model::PhysicalRange;

mod allowance;
mod configuration;
mod conversion;
pub mod pool;
mod region;
mod retirement;
mod search;

pub use super::allocation::{
    Allocation, AllocationConstruction, AllocationId, Cancellation, MappedStorage,
};
pub(crate) use super::allowance::permit::{AllocationPermit, AllocationPermitSlot, LayoutLimit};
pub use super::allowance::{
    Allowance, AllowanceCandidate, AllowanceId, Balance, Budget, BudgetObservation, BudgetProgress,
};
pub use super::bootstrap::{Config, Initial, RootStorage};
pub use super::returns::{Pending, Retired, ReturnId, Returned};
pub use super::search::{Lookup, LookupObservation};
pub use super::source::{Allocator, RegionId};
pub use region::{Grant, RegionDescription, RegionOrigin};
pub use retirement::ReturnReceipt;
pub use search::{Candidate, RamPurpose, Request, Search};

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
}
