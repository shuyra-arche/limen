//! Fixed physical sources and their local allocation and settlement records.

mod allocation;
mod allowance;
pub(crate) mod bootstrap;
mod directory;
mod pool;
// The independent target stack fixture exercises Buddy's native storage geometry.
pub mod region;
mod returns;
mod search;
mod source;
pub mod treaty;

use allocation::{Allocation, AllocationConstruction, AllocationId, MappedStorage};
use allowance::{Allowance, AllowanceId};
use bootstrap::Config;
use region::Region;
use returns::{Retired, ReturnId, Returned};
use source::{Allocator, RegionId, SourceId};
use treaty::{Candidate, Error, RamPurpose, RegionDescription, RegionOrigin};

#[cfg(test)]
#[path = "../../../../tests/metal/memory/allocator/mod.rs"]
mod tests;
