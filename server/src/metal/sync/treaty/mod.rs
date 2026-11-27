//! Synchronization operations for ordinary protected access and observation.

use crate::metal::processor::treaty::protect;

mod leaf;
mod snap;
mod turn;

pub use super::{
    leaf::{LeafError, LeafLock},
    snap::{Observation, PublishError, SnapLock, Snapshot, SnapshotBusy},
    turn::{
        TurnAccess, TurnAcquireError, TurnLock, TurnMode, TurnPlan, TurnPlanError, TurnWorkspace,
    },
};

/// Supplied node capacity on the current admitted CPU.
pub fn turn_capacity() -> Option<usize> {
    let protection = protect()?;
    Some(protection.synchronization()?.capacity())
}
