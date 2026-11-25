use core::{
    marker::PhantomData,
    ptr::NonNull,
    sync::atomic::{AtomicUsize, Ordering},
};

use crate::sync::{HeldLockSet, TurnLock};

use super::{Error, Occupied, SourceError, Vacant, group::State, place::Claim, with_turn};

/// Maximum simultaneous owners of a stable object, including its base owner.
pub const REFERENCE_LIMIT: usize = 64;

/// Opt-in stable storage. Only pools for stable values pay for the count.
pub struct Stable<T> {
    references: AtomicUsize,
    value: T,
}

impl<T> Stable<T> {
    pub const fn new(value: T) -> Self {
        Self {
            references: AtomicUsize::new(1),
            value,
        }
    }
}

/// The original base ownership after enabling references.
#[must_use]
pub struct StableOwner<'pool, 'storage, T>(Claim<'pool, 'storage, Stable<T>>);

/// Base ownership after the original owner has ended public lookup.
#[must_use]
pub struct Detached<'pool, 'storage, T>(Claim<'pool, 'storage, Stable<T>>);

/// The only right to destroy this detached value.
#[must_use]
pub struct Reclaim<'pool, 'storage, T>(Claim<'pool, 'storage, Stable<T>>);

/// One counted borrow of an object that cannot move or be reused.
#[must_use]
pub struct Reference<'pool, T> {
    stable: NonNull<Stable<T>>,
    lifetime: PhantomData<&'pool Stable<T>>,
}

// References only expose &T. Binding them to the Pool borrow keeps the
// mapping alive. The atomic count prohibits reuse until all have left.
unsafe impl<T: Send + Sync> Send for Reference<'_, T> {}
unsafe impl<T: Send + Sync> Sync for Reference<'_, T> {}

impl<'pool, 'storage, T> Occupied<'pool, 'storage, Stable<T>> {
    /// Enable counted references and keep the unique base ownership.
    pub fn stabilize(self) -> StableOwner<'pool, 'storage, T> {
        StableOwner(self.0)
    }
}

impl<'pool, 'storage, T> StableOwner<'pool, 'storage, T> {
    /// Call through the original published owner under its lookup protection.
    pub fn reference(&self) -> Result<Reference<'pool, T>, Error>
    where
        T: Sync,
    {
        // The unique base owner still keeps the initialized value live.
        let stable = unsafe { NonNull::new_unchecked(self.0.value()) };
        Reference::retain(stable)
    }

    /// The domain moves the base ownership out of its lookup position before
    /// handing it to retirement. Previously issued references remain valid.
    pub fn detach(self) -> Detached<'pool, 'storage, T> {
        Detached(self.0)
    }
}

impl<'pool, T> Reference<'pool, T> {
    pub fn get(&self) -> &T {
        // This reference holds a counted live interest, including through
        // detachment and independently of the scope of the lookup lock.
        unsafe { &self.stable.as_ref().value }
    }

    pub fn try_retain(&self) -> Result<Self, Error> {
        Self::retain(self.stable)
    }

    fn retain(stable: NonNull<Stable<T>>) -> Result<Self, Error> {
        // The caller still holds the base owner or a counted reference.
        let references = unsafe { &stable.as_ref().references };
        let current = references.load(Ordering::Relaxed);
        if current == 0 {
            return Err(Error::Stale);
        }
        if current >= REFERENCE_LIMIT {
            return Err(Error::References);
        }
        references
            .compare_exchange_weak(current, current + 1, Ordering::Relaxed, Ordering::Relaxed)
            .map_err(|_| Error::Busy)?;
        Ok(Self {
            stable,
            lifetime: PhantomData,
        })
    }
}

impl<T> Drop for Reference<'_, T> {
    fn drop(&mut self) {
        // All accesses through this reference end before the Release. After
        // the decrement the reclaimer may immediately destroy the object.
        unsafe {
            self.stable
                .as_ref()
                .references
                .fetch_sub(1, Ordering::Release)
        };
    }
}

impl<'pool, 'storage, T> Detached<'pool, 'storage, T> {
    /// Claim into the original exclusive recovery position.
    pub fn claim(
        self,
        receiver: &mut Option<Reclaim<'pool, 'storage, T>>,
    ) -> Result<(), (Self, Error)> {
        let mut detached = Some(self);
        let pool = detached.as_ref().expect("original detach").0.pool;
        match with_turn(&pool.state, |_| {
            detached.take().expect("single claim").claim_to(receiver)
        }) {
            Ok(result) => result,
            Err(error) => Err((detached.expect("no claim accepted"), error.into())),
        }
    }

    /// Claim and transfer the destruction right in the existing joint plan.
    pub fn claim_held<R>(
        self,
        held: &mut HeldLockSet<'_>,
        receiver: &TurnLock<R>,
        select: impl FnOnce(&mut R) -> &mut Option<Reclaim<'pool, 'storage, T>>,
    ) -> Result<(), (Self, Error)> {
        let Some((_state, receiver)) = held.access_pair(&self.0.pool.state, receiver) else {
            return Err((self, SourceError::Protection.into()));
        };
        self.claim_to(select(receiver))
    }

    fn claim_to(
        self,
        receiver: &mut Option<Reclaim<'pool, 'storage, T>>,
    ) -> Result<(), (Self, Error)> {
        if receiver.is_some() {
            return Err((self, Error::Receiver));
        }
        let stable = unsafe { &*self.0.value() };
        // The original lookup owner has been consumed. Existing references
        // can retain only while they keep a count above one. Acquire of the
        // final Release decrement orders all their access before destruction.
        if stable.references.load(Ordering::Acquire) != 1 {
            return Err((self, Error::References));
        }
        stable.references.store(0, Ordering::Relaxed);
        let slot = self.0.state();
        assert_eq!(slot.get(), State::Occupied);
        slot.set(State::Reclaiming);
        *receiver = Some(Reclaim(self.0));
        Ok(())
    }
}

impl<'pool, 'storage, T> Reclaim<'pool, 'storage, T> {
    pub fn destroy(self) -> Result<Vacant<'pool, 'storage, Stable<T>>, (Self, Error)> {
        if let Err(error) = self
            .0
            .pool
            .transition(&self.0, State::Reclaiming, State::Reclaiming)
        {
            return Err((self, error));
        }
        // This right excludes all old references and a second destructor.
        unsafe { self.0.value().drop_in_place() };
        Ok(Vacant(self.0))
    }
}
