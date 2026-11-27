//! Storage and private free-chain accounting for fixed identity slots.

use super::treaty::{Generation, Key, KeyError, Reservation, ReserveError, Slot};

pub(super) enum State<T> {
    Free { next: Option<Slot> },
    Reserved,
    Published(T),
    Detached(T),
    Reclaiming,
    Retired,
}

/// Caller-provided slot storage. Generation zero is never issued as a key.
pub(crate) struct Entry<T> {
    pub(super) generation: Option<Generation>,
    pub(super) state: State<T>,
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
    pub(super) scope: Scope,
    pub(super) entries: &'storage mut [Entry<T>],
    pub(super) free: Option<Slot>,
    pub(super) retired: usize,
}

impl<'storage, Scope: Copy + Eq, T> GenerationTable<'storage, Scope, T> {
    pub(super) fn locate(&self, key: Key<Scope>) -> Result<usize, KeyError> {
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

    pub(super) fn free_entry(&self, slot: Slot) -> (Option<Slot>, Generation) {
        let entry = &self.entries[slot.index()];
        let State::Free { next } = entry.state else {
            unreachable!("free chain contains only Free entries");
        };
        let generation =
            Generation::next(entry.generation).expect("exhausted slots are retired at release");
        (next, generation)
    }

    pub(super) fn reserve_front(&mut self, slot: Slot) -> Reservation<Scope> {
        let (next, generation) = self.free_entry(slot);
        self.entries[slot.index()] = Entry {
            generation: Some(generation),
            state: State::Reserved,
        };
        self.free = next;
        Reservation(Key::new(self.scope, slot, generation))
    }

    pub(super) fn unavailable(&self) -> ReserveError {
        // A full table can contain live or reclaimable entries. Exhaustion
        // requires a nonempty table with every slot permanently retired.
        if self.retired != 0 && self.retired == self.entries.len() {
            ReserveError::IdentityExhausted
        } else {
            ReserveError::Capacity
        }
    }
}
