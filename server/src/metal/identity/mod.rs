// Finite identities for owner-supplied storage and protection.
use core::{mem, num::NonZeroU32};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Slot(u32);

impl Slot {
    pub(crate) fn new(index: usize) -> Option<Self> {
        u32::try_from(index).ok().map(Self)
    }

    pub(crate) fn index(self) -> usize {
        usize::try_from(self.0).expect("issued slot fits the machine")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Generation(NonZeroU32);

impl Generation {
    pub(crate) const FIRST: Self = Self(NonZeroU32::MIN);

    pub(crate) fn next(previous: Option<Self>) -> Option<Self> {
        match previous {
            Some(previous) => previous.0.checked_add(1).map(Self),
            None => Some(Self::FIRST),
        }
    }

    pub(crate) fn exhausted(self) -> bool {
        self.0.get() == u32::MAX
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Key<Scope> {
    scope: Scope,
    slot: Slot,
    generation: Generation,
}

impl<Scope: Copy> Key<Scope> {
    pub(crate) const fn new(scope: Scope, slot: Slot, generation: Generation) -> Self {
        Self {
            scope,
            slot,
            generation,
        }
    }

    pub(crate) fn scope(self) -> Scope {
        self.scope
    }

    pub(crate) fn slot(self) -> Slot {
        self.slot
    }

    pub(crate) fn generation(self) -> Generation {
        self.generation
    }
}

#[derive(Debug)]
#[must_use = "the owner must publish or cancel this reservation"]
pub(crate) struct Reservation<Scope>(Key<Scope>);

impl<Scope: Copy> Reservation<Scope> {
    pub(crate) fn key(&self) -> Key<Scope> {
        self.0
    }
}

#[derive(Debug)]
#[must_use = "the owner must settle and release this slot"]
pub(crate) struct Reclamation<Scope>(Key<Scope>);

impl<Scope: Copy> Reclamation<Scope> {
    pub(crate) fn key(&self) -> Key<Scope> {
        self.0
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum InitializeError {
    Capacity,
    UsedStorage,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum ReserveError {
    Capacity,
    IdentityExhausted,
    OutputOccupied,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum KeyError {
    WrongScope,
    OutsideTable,
    Stale,
    WrongState,
}

enum State<T> {
    Free { next: Option<Slot> },
    Reserved,
    Published(T),
    Detached(T),
    Reclaiming,
    Retired,
}

/// Caller-provided slot storage. Generation zero is never issued as a key.
pub(crate) struct Entry<T> {
    generation: Option<Generation>,
    state: State<T>,
}

impl<T> Entry<T> {
    pub(crate) const EMPTY: Self = Self {
        generation: None,
        state: State::Free { next: None },
    };
}

/// Fixed identity slots under the actual owner's protection. The owner retains
/// the preparation and settlement obligations associated with linear tokens.
pub(crate) struct GenerationTable<'storage, Scope, T> {
    scope: Scope,
    entries: &'storage mut [Entry<T>],
    free: Option<Slot>,
    retired: usize,
}

impl<'storage, Scope: Copy + Eq, T> GenerationTable<'storage, Scope, T> {
    /// Use freshly supplied storage and the actual table owner's namespace.
    /// The owner constructs its scope once and keeps it distinct from all
    /// other interpretable tables of this type, including retired instances.
    /// The typed slice already establishes storage length, layout and alignment.
    /// The owner initializes each slot with `Entry::EMPTY` before construction.
    pub(crate) fn new(
        scope: Scope,
        entries: &'storage mut [Entry<T>],
    ) -> Result<Self, InitializeError> {
        if Slot::new(entries.len().saturating_sub(1)).is_none() {
            return Err(InitializeError::Capacity);
        }
        if entries.iter().any(|entry| entry.generation.is_some()) {
            return Err(InitializeError::UsedStorage);
        }
        let mut free = None;
        for (index, entry) in entries.iter_mut().enumerate().rev() {
            entry.state = State::Free { next: free };
            free = Some(Slot::new(index).expect("table capacity checked"));
        }
        Ok(Self {
            scope,
            entries,
            free,
            retired: 0,
        })
    }

    pub(crate) fn reserve(&mut self) -> Result<Reservation<Scope>, ReserveError> {
        let slot = self.free.ok_or_else(|| self.unavailable())?;
        Ok(self.reserve_front(slot))
    }

    /// Observe available slots under the current protection without claiming any.
    pub(crate) fn check_reserve(&self, count: usize) -> Result<(), ReserveError> {
        if count > self.entries.len() {
            return Err(ReserveError::Capacity);
        }
        let mut front = self.free;
        for _ in 0..count {
            let slot = front.ok_or_else(|| self.unavailable())?;
            (front, _) = self.free_entry(slot);
        }
        Ok(())
    }

    /// An owner can preflight multiple tables and their outputs while holding
    /// the same joint protection across the checks and commits.
    pub(crate) fn check_reserve_many(
        &self,
        output: &[Option<Reservation<Scope>>],
    ) -> Result<(), ReserveError> {
        self.check_reserve(output.len())?;
        if output.iter().any(Option::is_some) {
            return Err(ReserveError::OutputOccupied);
        }
        Ok(())
    }

    pub(crate) fn reserve_many(
        &mut self,
        output: &mut [Option<Reservation<Scope>>],
    ) -> Result<(), ReserveError> {
        self.check_reserve_many(output)?;
        for place in output {
            let slot = self.free.expect("all slots checked in this borrow");
            *place = Some(self.reserve_front(slot));
        }
        Ok(())
    }

    pub(crate) fn publish(
        &mut self,
        reservation: Reservation<Scope>,
        value: T,
    ) -> Result<Key<Scope>, (Reservation<Scope>, T, KeyError)> {
        let key = reservation.key();
        match self.locate(key) {
            Ok(index) if matches!(self.entries[index].state, State::Reserved) => {
                self.entries[index].state = State::Published(value);
                Ok(key)
            }
            Ok(_) => Err((reservation, value, KeyError::WrongState)),
            Err(reason) => Err((reservation, value, reason)),
        }
    }

    // Allocator publishes its business-phase reservation immediately. Owners
    // with private identity preparation can still cancel before publication.
    #[allow(dead_code)]
    pub(crate) fn cancel(
        &mut self,
        reservation: Reservation<Scope>,
    ) -> Result<Reclamation<Scope>, (Reservation<Scope>, KeyError)> {
        let key = reservation.key();
        match self.locate(key) {
            Ok(index) if matches!(self.entries[index].state, State::Reserved) => {
                self.entries[index].state = State::Reclaiming;
                Ok(Reclamation(key))
            }
            Ok(_) => Err((reservation, KeyError::WrongState)),
            Err(reason) => Err((reservation, reason)),
        }
    }

    pub(crate) fn get(&self, key: Key<Scope>) -> Option<&T> {
        let index = self.locate(key).ok()?;
        match &self.entries[index].state {
            State::Published(value) => Some(value),
            _ => None,
        }
    }

    pub(crate) fn capacity(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn published_at(&self, index: usize) -> Option<(Key<Scope>, &T)> {
        let entry = self.entries.get(index)?;
        let State::Published(value) = &entry.state else {
            return None;
        };
        let generation = entry.generation.expect("published slots have a generation");
        let slot = Slot::new(index).expect("table capacity checked");
        Some((Key::new(self.scope, slot, generation), value))
    }

    pub(crate) fn get_mut(&mut self, key: Key<Scope>) -> Option<&mut T> {
        let index = self.locate(key).ok()?;
        match &mut self.entries[index].state {
            State::Published(value) => Some(value),
            _ => None,
        }
    }

    /// Close ordinary lookup. The entry retains the original value and duties.
    pub(crate) fn detach(&mut self, key: Key<Scope>) -> Result<(), KeyError> {
        let index = self.locate(key)?;
        let state = &mut self.entries[index].state;
        if !matches!(state, State::Published(_)) {
            return Err(KeyError::WrongState);
        }
        let State::Published(value) = mem::replace(state, State::Reclaiming) else {
            unreachable!("validated published entry");
        };
        *state = State::Detached(value);
        Ok(())
    }

    /// The owner checks that external borrowers are gone and can take over the
    /// value. Fixed objects with external borrows must use a stable locator as T.
    pub(crate) fn reclaim(&mut self, key: Key<Scope>) -> Result<(Reclamation<Scope>, T), KeyError> {
        let index = self.locate(key)?;
        let state = &mut self.entries[index].state;
        if !matches!(state, State::Detached(_)) {
            return Err(KeyError::WrongState);
        }
        let State::Detached(value) = mem::replace(state, State::Reclaiming) else {
            unreachable!("validated detached entry");
        };
        Ok((Reclamation(key), value))
    }

    /// Release after the owner settles the original preparation or object.
    pub(crate) fn release(
        &mut self,
        reclamation: Reclamation<Scope>,
    ) -> Result<(), (Reclamation<Scope>, KeyError)> {
        let key = reclamation.key();
        let index = match self.locate(key) {
            Ok(index) => index,
            Err(reason) => return Err((reclamation, reason)),
        };
        let entry = &mut self.entries[index];
        if !matches!(entry.state, State::Reclaiming) {
            return Err((reclamation, KeyError::WrongState));
        }
        if key.generation().exhausted() {
            entry.state = State::Retired;
            self.retired += 1;
        } else {
            entry.state = State::Free { next: self.free };
            self.free = Some(key.slot());
        }
        Ok(())
    }

    fn locate(&self, key: Key<Scope>) -> Result<usize, KeyError> {
        if self.scope != key.scope() {
            return Err(KeyError::WrongScope);
        }
        let index = key.slot().index();
        let entry = self.entries.get(index).ok_or(KeyError::OutsideTable)?;
        if entry.generation != Some(key.generation()) {
            return Err(KeyError::Stale);
        }
        Ok(index)
    }

    fn free_entry(&self, slot: Slot) -> (Option<Slot>, Generation) {
        let entry = &self.entries[slot.index()];
        let State::Free { next } = entry.state else {
            unreachable!("free chain contains only Free entries");
        };
        let generation =
            Generation::next(entry.generation).expect("exhausted slots are retired at release");
        (next, generation)
    }

    fn reserve_front(&mut self, slot: Slot) -> Reservation<Scope> {
        let (next, generation) = self.free_entry(slot);
        self.entries[slot.index()] = Entry {
            generation: Some(generation),
            state: State::Reserved,
        };
        self.free = next;
        Reservation(Key::new(self.scope, slot, generation))
    }

    fn unavailable(&self) -> ReserveError {
        // A full table can contain live or reclaimable entries. Exhaustion
        // requires a nonempty table with every slot permanently retired.
        if self.retired != 0 && self.retired == self.entries.len() {
            ReserveError::IdentityExhausted
        } else {
            ReserveError::Capacity
        }
    }
}
