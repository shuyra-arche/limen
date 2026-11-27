//! Synchronization for trusted code linked into the kernel image.
//!
//! Lock acquisition is available after the boot entry establishes local
//! protection for the current Hart. Synchronous exception entry cannot use it.

mod leaf;
mod snap;
mod turn;

pub use leaf::{LeafError, LeafLock};
pub use snap::{Observation, PublishError, SnapLock, Snapshot, SnapshotBusy};
pub use turn::{
    TurnAccess, TurnAcquireError, TurnLock, TurnMode, TurnPlan, TurnPlanError, TurnWorkspace,
};

pub use crate::metal::processor::hart::local::turn_capacity;

/// Default node supply selected by the current trusted bootstrap image.
pub const TURN_CAPACITY: usize = 5;
