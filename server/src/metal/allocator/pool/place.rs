use core::{cell::Cell, ptr::NonNull};

use crate::sync::{HeldLockSet, TurnLock};

use super::{
    Error, GroupId, Pool, SourceError,
    group::{Slot, State},
    with_turn,
};

pub(super) struct Claim<'pool, 'storage, T> {
    pool: &'pool Pool<'storage, T>,
    pub(super) group: GroupId,
    pub(super) index: usize,
    slot: NonNull<Slot<T>>,
}

// The owning token is unique, its borrow pins the Pool and mapping, and
// the value cannot have live borrows when that token moves. State remains
// under the Pool Turn. Moving the token moves the right to access and drop T.
unsafe impl<T: Send> Send for Claim<'_, '_, T> {}

impl<'pool, 'storage, T> Claim<'pool, 'storage, T> {
    pub(super) fn new(
        pool: &'pool Pool<'storage, T>,
        group: GroupId,
        index: usize,
        slot: NonNull<Slot<T>>,
    ) -> Self {
        Self {
            pool,
            group,
            index,
            slot,
        }
    }

    // The caller holds the originating Pool Turn.
    pub(super) fn state(&self) -> &Cell<State> {
        // The token holds the original Pool and the fixed, live slot.
        unsafe { &self.slot.as_ref().state }
    }

    fn value(&self) -> *mut T {
        // The linear token gives exclusive value access. Inactive slots are
        // never exposed as &T and the state field is disjoint.
        unsafe { self.slot.as_ref().value.get().cast::<T>() }
    }
}

/// Unique uninitialized slot in the original preparation.
#[must_use = "the original owner must initialize or release this slot"]
pub struct Reservation<'pool, 'storage, T>(pub(super) Claim<'pool, 'storage, T>);

/// The original preparation's valid object, still private.
#[must_use = "the original owner must publish or destroy its prepared object"]
pub struct Prepared<'pool, 'storage, T>(Claim<'pool, 'storage, T>);

/// The consumer's unique typed object. Borrowing does not leave this owner.
#[must_use = "the original owner must destroy and settle this object"]
pub struct Occupied<'pool, 'storage, T>(Claim<'pool, 'storage, T>);

/// The original object has been destroyed. Its slot stays claimed until settled.
#[must_use = "the original owner must release this destroyed slot"]
pub struct Vacant<'pool, 'storage, T>(Claim<'pool, 'storage, T>);

impl<'pool, 'storage, T> Reservation<'pool, 'storage, T> {
    /// Write a supported value into the final slot. The Pool layout bounds
    /// this by-value transfer. Callers still budget constructing T itself.
    pub fn initialize(self, object: T) -> Prepared<'pool, 'storage, T> {
        // The reservation was removed from the free chain. Only it writes T.
        unsafe { self.0.value().write(object) };
        Prepared(self.0)
    }

    pub fn cancel(self) -> Result<(), (Self, Error)> {
        self.0
            .pool
            .release(&self.0, State::Reserved)
            .map_err(|error| (self, error))
    }
}

impl<'pool, 'storage, T> Prepared<'pool, 'storage, T> {
    /// Transfer this object to its durable consumer position under the Turn.
    /// A rejected handoff returns the original prepared object.
    pub fn publish(
        self,
        receiver: &mut Option<Occupied<'pool, 'storage, T>>,
    ) -> Result<(), (Self, Error)> {
        if receiver.is_some() {
            return Err((self, Error::Receiver));
        }
        let pool = self.0.pool;
        let mut prepared = Some(self);
        match with_turn(&pool.state, |_| {
            prepared
                .take()
                .expect("single publication")
                .publish_to(receiver)
        }) {
            Ok(result) => result,
            Err(error) => Err((
                prepared.expect("preparation was not accepted"),
                Error::from(error),
            )),
        }
    }

    /// Publish directly into a protected consumer position, using its lock
    /// and the original Pool from this same held plan.
    pub fn publish_held<R>(
        self,
        held: &mut HeldLockSet<'_>,
        receiver: &TurnLock<R>,
        select: impl FnOnce(&mut R) -> &mut Option<Occupied<'pool, 'storage, T>>,
    ) -> Result<(), (Self, Error)> {
        let Some((_state, receiver)) = held.access_pair(&self.0.pool.state, receiver) else {
            return Err((self, SourceError::Protection.into()));
        };
        self.publish_to(select(receiver))
    }

    fn publish_to(
        self,
        receiver: &mut Option<Occupied<'pool, 'storage, T>>,
    ) -> Result<(), (Self, Error)> {
        if receiver.is_some() {
            return Err((self, Error::Receiver));
        }
        let claim = self.0;
        let state = claim.state();
        assert_eq!(state.get(), State::Reserved);
        state.set(State::Occupied);
        *receiver = Some(Occupied(claim));
        Ok(())
    }

    pub fn destroy(self) -> Result<Reservation<'pool, 'storage, T>, (Self, Error)> {
        // Check the ordinary execution context before running a destructor.
        // The original preparation retains exclusive ownership throughout.
        if let Err(error) = self
            .0
            .pool
            .transition(&self.0, State::Reserved, State::Reserved)
        {
            return Err((self, error));
        }
        unsafe { self.0.value().drop_in_place() };
        Ok(Reservation(self.0))
    }
}

impl<'pool, 'storage, T> Occupied<'pool, 'storage, T> {
    pub fn get(&self) -> &T {
        // A published slot is initialized. Its owner cannot be consumed while
        // this borrow lasts.
        unsafe { &*self.0.value() }
    }

    pub fn destroy(self) -> Result<Vacant<'pool, 'storage, T>, (Self, Error)> {
        if let Err(error) = self
            .0
            .pool
            .transition(&self.0, State::Occupied, State::Reclaiming)
        {
            return Err((self, error));
        }
        // The Turn was released and this token owns all references to T.
        // A reclaiming slot cannot be acquired until this destructor returns.
        unsafe { self.0.value().drop_in_place() };
        Ok(Vacant(self.0))
    }
}

impl<T> Vacant<'_, '_, T> {
    pub fn release(self) -> Result<(), (Self, Error)> {
        self.0
            .pool
            .release(&self.0, State::Reclaiming)
            .map_err(|error| (self, error))
    }
}
