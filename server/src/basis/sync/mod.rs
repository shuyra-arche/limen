//! Synchronization for trusted code linked into the kernel image.
//!
//! Lock acquisition is available after the boot entry establishes local
//! protection for the current Hart. Synchronous exception entry cannot use it.

mod leaf;
mod queue;
mod snap;
mod turn;

pub use leaf::{LeafError, LeafLock};
pub use snap::{Observation, PublishError, SnapLock, Snapshot, SnapshotBusy};
pub use turn::{Acquisition, AcquisitionError, HeldLockSet, LockPlan, PlanError, TurnLock, Work};
