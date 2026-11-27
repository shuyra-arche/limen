//! Ordinary typed storage admission and exact supply observations.

use crate::metal::sync::treaty::{TurnAccess, TurnLock, TurnPlan, TurnPlanError};

pub use super::super::pool::{
    Capacity, DestructionReceipt, Detached, Error, Geometry, GroupId, GroupReceipt, GroupSlot,
    Growth, Occupied, Pool, Prepared, REFERENCE_LIMIT, Reclamation, Reference, ReleaseCursor,
    ReleaseProgress, Reservation, Retirement, Stable, StableOwner, Supply, VALUE_ALIGNMENT,
    VALUE_BYTES,
};
use super::Error as SourceError;

impl<'storage, T> Pool<'storage, T> {
    pub fn capacity(&self) -> Result<Capacity, Error> {
        Ok(self.state.acquire(|state| Capacity {
            groups: state.open,
            bytes: state.bytes,
            slots: state.capacity,
            free: state.free,
        })?)
    }

    /// Include this Pool in the complete plan for a shared handoff.
    pub fn plan<'pool>(&'pool self, plan: &mut TurnPlan<'pool, '_>) -> Result<(), TurnPlanError> {
        plan.insert(&self.state)
    }

    /// The caller supplies the original, exclusively borrowed preparation
    /// position. Use reserve_held when that position requires a Turn.
    pub fn reserve<'pool>(
        &'pool self,
        receiver: &mut Option<Reservation<'pool, 'storage, T>>,
    ) -> Result<(), Error> {
        if receiver.is_some() {
            return Err(Error::Receiver);
        }
        self.state
            .acquire(|state| self.reserve_to(state, receiver))?
    }

    /// Commit a reservation and its protected preparation record together.
    /// The caller must retain this original record through suspension or loss
    /// of the response. The selector runs with both locks held.
    pub fn reserve_held<'pool, R>(
        &'pool self,
        held: &mut TurnAccess<'_>,
        receiver: &TurnLock<R>,
        select: impl FnOnce(&mut R) -> &mut Option<Reservation<'pool, 'storage, T>>,
    ) -> Result<(), Error> {
        let (state, receiver) = held
            .access_pair(&self.state, receiver)
            .ok_or(SourceError::Protection)?;
        self.reserve_to(state, select(receiver))
    }
}
