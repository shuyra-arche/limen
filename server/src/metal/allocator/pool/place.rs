use core::{cell::Cell, ptr::NonNull};

use super::{
    Error, GroupId, Pool,
    group::{Slot, State},
};

pub(super) struct Claim<'pool, 'storage, T> {
    pool: &'pool Pool<'storage, T>,
    pub(super) group: GroupId,
    pub(super) index: usize,
    slot: NonNull<Slot<T>>,
}

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
        // Deposit the unique owner while the corresponding slot becomes visible.
        pool.publish(self, receiver)
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

impl<'storage, T> Pool<'storage, T> {
    fn publish<'pool>(
        &'pool self,
        prepared: Prepared<'pool, 'storage, T>,
        receiver: &mut Option<Occupied<'pool, 'storage, T>>,
    ) -> Result<(), (Prepared<'pool, 'storage, T>, Error)> {
        // The closure retains the original prepared token on acquisition error.
        let mut prepared = Some(prepared);
        let result = super::with_turn(&self.state, |_| {
            let claim = prepared.take().expect("single publication").0;
            let state = claim.state();
            assert_eq!(state.get(), State::Reserved);
            state.set(State::Occupied);
            *receiver = Some(Occupied(claim));
        });
        result.map_err(|error| {
            (
                prepared.expect("preparation was not accepted"),
                Error::from(error),
            )
        })
    }
}
