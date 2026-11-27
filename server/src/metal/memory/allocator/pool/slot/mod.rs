//! Slot storage and the original rights to prepare, destroy, and release it.
mod reference;

pub use reference::{Detached, REFERENCE_LIMIT, Reclamation, Reference, Stable, StableOwner};

use core::{
    cell::UnsafeCell,
    mem::MaybeUninit,
    ptr::NonNull,
    sync::atomic::{AtomicUsize, Ordering},
};

use crate::metal::sync::{TurnAccess, TurnLock};

use super::{Error, GroupId, GroupRecord, Pool, SourceError};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum State {
    Free(Option<usize>),
    Reserved,
    Occupied,
    Reclaiming,
    Destroyed(Completion),
}

const DESTROYED: usize = 1 << (usize::BITS - 1);

/// A completion number issued once by the original Pool.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Completion(usize);

impl Completion {
    pub(super) const FIRST: Self = Self(1);
    pub(super) const UNTRACKED: Self = Self(0);

    pub(super) fn take(&mut self) -> Self {
        if self.0 == DESTROYED {
            // Leave slot reuse available through recovery after the direct
            // completion numbers have been exhausted.
            return Self::UNTRACKED;
        }
        let issued = *self;
        self.0 += 1;
        issued
    }
}

/// The Pool Turn protects state transitions and the free chain. The unique
/// destroyer publishes completion with Release outside the Pool Turn.
pub(super) struct SlotState(AtomicUsize);

impl SlotState {
    pub(super) fn new(state: State) -> Self {
        Self(AtomicUsize::new(Self::encode(state)))
    }

    fn encode(state: State) -> usize {
        match state {
            State::Free(None) => 0,
            State::Reserved => 1,
            State::Occupied => 2,
            State::Reclaiming => 3,
            State::Destroyed(completion) => DESTROYED | completion.0,
            // The validated array fits in isize and each slot holds an atomic
            // word, so adding the phase offset leaves the top bit clear.
            State::Free(Some(next)) => next + 4,
        }
    }

    pub(super) fn get(&self) -> State {
        match self.0.load(Ordering::Acquire) {
            0 => State::Free(None),
            1 => State::Reserved,
            2 => State::Occupied,
            3 => State::Reclaiming,
            completed if completed & DESTROYED != 0 => {
                State::Destroyed(Completion(completed & !DESTROYED))
            }
            next => State::Free(Some(next - 4)),
        }
    }

    pub(super) fn set(&self, state: State) {
        self.0.store(Self::encode(state), Ordering::Relaxed);
    }

    pub(super) fn complete_destruction(&self, completion: Completion) {
        self.0.store(
            Self::encode(State::Destroyed(completion)),
            Ordering::Release,
        );
    }
}

/// Neither a slot transition nor an object access borrows the group bytes
/// as an exclusive slice after publication.
pub(super) struct Slot<T> {
    pub(super) state: SlotState,
    pub(super) value: UnsafeCell<MaybeUninit<T>>,
}

/// A slot index within one exact generation of a Pool group.
#[derive(Clone, Copy)]
pub(super) struct SlotPosition {
    pub(super) group: GroupId,
    pub(super) index: usize,
}

pub(super) struct Claim<'pool, 'storage, T> {
    pub(super) pool: &'pool Pool<'storage, T>,
    position: SlotPosition,
    slot: NonNull<Slot<T>>,
}

// The owning token is unique, its borrow pins the Pool and mapping, and
// the value cannot have live borrows when that token moves. State remains
// under the Pool Turn. Moving the token moves the right to access and drop T.
unsafe impl<T: Send> Send for Claim<'_, '_, T> {}

impl<'pool, 'storage, T> Claim<'pool, 'storage, T> {
    pub(super) fn new(
        pool: &'pool Pool<'storage, T>,
        position: SlotPosition,
        slot: NonNull<Slot<T>>,
    ) -> Self {
        Self {
            pool,
            position,
            slot,
        }
    }

    // State transitions require the originating Turn or the unique right
    // to publish the destructor's completion.
    pub(super) fn state(&self) -> &SlotState {
        // The token holds the original Pool and the fixed, live slot.
        unsafe { &self.slot.as_ref().state }
    }

    pub(super) fn value(&self) -> *mut T {
        // The linear token gives exclusive value access. Inactive slots are
        // never exposed as &T and the state field is disjoint.
        unsafe { self.slot.as_ref().value.get().cast::<T>() }
    }

    pub(super) fn destroy(
        self,
        from: State,
    ) -> Result<DestructionReceipt<'pool, 'storage, T>, (Self, Error)> {
        let completion = match self.pool.state.acquire(|directory| {
            let slot = self.state();
            assert_eq!(slot.get(), from);
            slot.set(State::Reclaiming);
            directory.next_completion.take()
        }) {
            Ok(completion) => completion,
            Err(error) => return Err((self, error.into())),
        };
        let receipt = DestructionReceipt {
            pool: self.pool,
            position: self.position,
            completion,
        };
        // The unique claim excludes all borrows and a second destroyer.
        unsafe { self.value().drop_in_place() };
        // This is the final mapped-slot access. A recovery worker may release
        // the slot, and the group may shut, before the receipt is returned.
        self.state().complete_destruction(completion);
        Ok(receipt)
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
pub struct Occupied<'pool, 'storage, T>(pub(super) Claim<'pool, 'storage, T>);

/// Direct receipt for one completed destructor. The Pool can also recover
/// the slot independently. This receipt keeps no pointer to the slot's bytes.
#[must_use = "release the slot directly or let the Pool recover it"]
pub struct DestructionReceipt<'pool, 'storage, T> {
    pool: &'pool Pool<'storage, T>,
    position: SlotPosition,
    completion: Completion,
}

impl<'pool, 'storage, T> Reservation<'pool, 'storage, T> {
    /// Write a supported value into the final slot. The Pool layout bounds
    /// this by-value transfer. Callers still budget constructing T itself.
    pub fn initialize(self, object: T) -> Prepared<'pool, 'storage, T> {
        // The reservation was removed from the free chain. Only it writes T.
        unsafe { self.0.value().write(object) };
        Prepared(self.0)
    }

    pub fn cancel(self) -> Result<(), (Self, Error)> {
        let result = self
            .0
            .pool
            .state
            .acquire(|directory| directory.release(self.0.position, State::Reserved));
        result
            .map_err(Error::from)
            .flatten()
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
        match pool.state.acquire(|_| {
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
        held: &mut TurnAccess<'_>,
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
        if let Err(error) = self.0.pool.state.acquire(|_| {
            assert_eq!(self.0.state().get(), State::Reserved);
        }) {
            return Err((self, error.into()));
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

    pub fn destroy(self) -> Result<DestructionReceipt<'pool, 'storage, T>, (Self, Error)> {
        self.0
            .destroy(State::Occupied)
            .map_err(|(claim, error)| (Self(claim), error))
    }
}

impl<T> DestructionReceipt<'_, '_, T> {
    /// Settle the original slot directly under the Pool Turn. If recovery
    /// already settled it, return Stale without touching any later lifetime.
    /// Exhausting the Pool's non-reusing numbers leaves scan recovery available.
    pub fn release(self) -> Result<(), (Self, Error)> {
        if self.completion == Completion::UNTRACKED {
            return Err((self, SourceError::IdentityExhausted.into()));
        }
        let result = self.pool.state.acquire(|directory| {
            directory.release(self.position, State::Destroyed(self.completion))
        });
        result
            .map_err(Error::from)
            .flatten()
            .map_err(|error| (self, error))
    }
}

/// A recovery pass needs only the Pool root and a fresh cursor.
#[derive(Default)]
pub struct ReleaseCursor {
    group: usize,
    slot: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReleaseProgress {
    Released,
    More,
    End,
}

impl<T> Pool<'_, T> {
    /// Settle at most one destroyed slot. Inspect at most `budget` directory
    /// steps or slots (capped at 64). End resets the cursor for another pass.
    /// A failed acquisition leaves the cursor intact.
    pub fn release_destroyed(
        &self,
        cursor: &mut ReleaseCursor,
        budget: usize,
    ) -> Result<ReleaseProgress, Error> {
        Ok(self.state.acquire(|state| {
            for _ in 0..budget.min(64) {
                if cursor.group >= state.extent {
                    *cursor = ReleaseCursor::default();
                    return ReleaseProgress::End;
                }
                if let Some(GroupRecord::Open(group)) = &state.groups[cursor.group].record
                    && cursor.slot < group.geometry.slots
                {
                    let index = cursor.slot;
                    cursor.slot += 1;
                    // The Turn keeps this mapping live. Acquire observes the
                    // destructor's normal completion before the slot is reused.
                    if let State::Destroyed(completion) =
                        unsafe { group.slot(index).as_ref().state.get() }
                    {
                        let position = SlotPosition {
                            group: GroupId(cursor.group, group.material.allocation.id()),
                            index,
                        };
                        state
                            .release(position, State::Destroyed(completion))
                            .expect("completion observed under the same Turn");
                        return ReleaseProgress::Released;
                    }
                } else {
                    cursor.group += 1;
                    cursor.slot = 0;
                }
            }
            ReleaseProgress::More
        })?)
    }
}
