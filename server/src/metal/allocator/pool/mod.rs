//! Fixed, mapped storage for a consumer's typed objects.

mod group;
mod place;
mod reference;
mod retirement;

use crate::sync::{HeldLockSet, LockPlan, PlanError, TurnLock};

use super::{AllocationId, Error as SourceError, MappedStorage, with_turn};
use group::{Chain, Completion, Group, Links, State};

pub use group::{Geometry, Supply};
use place::Claim;
pub use place::{Occupied, Prepared, ReleaseCursor, ReleaseProgress, Reservation, Vacant};
pub use reference::{Detached, REFERENCE_LIMIT, Reclaim, Reference, Stable, StableOwner};
use retirement::GroupRecord;
pub use retirement::Retirement;

/// Largest value supported by this increment's by-value initializer.
pub const VALUE_BYTES: usize = 512;
/// Largest alignment supported by the by-value initializer, including ZSTs.
pub const VALUE_ALIGNMENT: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Directory,
    Layout,
    Material,
    Receiver,
    Full,
    Busy,
    References,
    Stale,
    Source(SourceError),
}

impl From<SourceError> for Error {
    fn from(error: SourceError) -> Self {
        Self::Source(error)
    }
}

/// A fixed directory position qualified by its original Allocation.
/// A returned position is not reused in this fixed pool.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GroupId(usize, AllocationId);

/// Storage supplied by the actual consumer outside its reclaimable pages.
pub struct GroupSlot<'storage, T>(Option<GroupRecord<'storage, T>>);

impl<T> GroupSlot<'_, T> {
    pub const EMPTY: Self = Self(None);
}

pub struct Pool<'storage, T> {
    state: TurnLock<Directory<'storage, T>>,
}

struct Directory<'storage, T> {
    groups: &'storage mut [GroupSlot<'storage, T>],
    count: usize,
    open: usize,
    bytes: usize,
    capacity: usize,
    free: usize,
    next_completion: Completion,
    empty: Option<GroupId>,
    partial: Option<GroupId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Capacity {
    /// Groups open to new slot claims.
    pub groups: usize,
    /// Exact physical charge still carried by this Pool's records.
    pub bytes: usize,
    pub slots: usize,
    pub free: usize,
}

impl<'storage, T> Pool<'storage, T> {
    /// Install groups through the unique root before publishing the Pool. The
    /// directory and root live outside all supplied group mappings.
    pub fn new(groups: &'storage mut [GroupSlot<'storage, T>]) -> Result<Self, Error> {
        if groups.is_empty() || groups.iter().any(|entry| entry.0.is_some()) {
            return Err(Error::Directory);
        }
        Ok(Self {
            state: TurnLock::new(Directory {
                groups,
                count: 0,
                open: 0,
                bytes: 0,
                capacity: 0,
                free: 0,
                next_completion: Completion::FIRST,
                empty: None,
                partial: None,
            }),
        })
    }

    /// Install the actual paired material. Every rejection preserves the
    /// original pair and directory. The successful group retains its source,
    /// exact mapping and independent retirement position.
    ///
    /// # Safety
    /// Keep the independent directory and root valid outside all supplied
    /// group mappings until all upstream return and receipt work has ended.
    /// The actual mapped storage owner must preserve its mapping/exclusivity
    /// obligations through the group's last use, including typed references.
    pub unsafe fn install(
        &mut self,
        material: &mut Option<MappedStorage<'storage>>,
    ) -> Result<GroupId, Error> {
        let state = self.state.get_mut();
        if state.count == state.groups.len() {
            return Err(Error::Directory);
        }
        let supplied = material.as_ref().ok_or(Error::Material)?;
        let id = GroupId(state.count, supplied.id());
        let mapping = supplied.mapping();
        let geometry = Geometry::of::<T>(mapping.length())?;
        if !mapping.address().is_multiple_of(geometry.alignment)
            || !supplied
                .physical()
                .start
                .is_multiple_of(geometry.alignment as u64)
        {
            return Err(Error::Layout);
        }
        let capacity = state
            .capacity
            .checked_add(geometry.slots)
            .ok_or(Error::Directory)?;
        let bytes = state
            .bytes
            .checked_add(mapping.length())
            .ok_or(Error::Directory)?;
        let retirement = supplied.allocation().return_position()?;

        // The original owner keeps the complete pair throughout the slot
        // walk. Only the short directory publication consumes it.
        Group::<T>::prepare(
            geometry,
            material
                .as_mut()
                .expect("checked material")
                .mapping_mut()
                .bytes(),
        );
        let (allocation, mapping) = material.take().expect("checked material").into_parts();
        let group = Group::new(allocation, retirement, geometry, mapping.into_bytes());
        state.groups[id.0].0 = Some(GroupRecord::Open(group));
        state.count += 1;
        state.open += 1;
        state.capacity = capacity;
        state.free += geometry.slots;
        state.bytes = bytes;
        state.link(id, Chain::Empty);
        Ok(id)
    }

    pub fn capacity(&self) -> Result<Capacity, Error> {
        Ok(with_turn(&self.state, |state| Capacity {
            groups: state.open,
            bytes: state.bytes,
            slots: state.capacity,
            free: state.free,
        })?)
    }

    pub fn supply(&self, id: GroupId) -> Result<Supply, Error> {
        with_turn(&self.state, |state| match state.entry(id)? {
            GroupRecord::Open(group) => Ok(group.supply()),
            _ => Err(Error::Stale),
        })?
    }

    /// Include this Pool in the complete plan for a shared handoff.
    pub fn plan<'pool>(&'pool self, plan: &mut LockPlan<'pool, '_>) -> Result<(), PlanError> {
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
        with_turn(&self.state, |state| self.reserve_to(state, receiver))?
    }

    /// Commit a reservation and its protected preparation record together.
    /// The caller must retain this original record through suspension or loss
    /// of the response. The selector runs with both locks held.
    pub fn reserve_held<'pool, R>(
        &'pool self,
        held: &mut HeldLockSet<'_>,
        receiver: &TurnLock<R>,
        select: impl FnOnce(&mut R) -> &mut Option<Reservation<'pool, 'storage, T>>,
    ) -> Result<(), Error> {
        let (state, receiver) = held
            .access_pair(&self.state, receiver)
            .ok_or(SourceError::Protection)?;
        self.reserve_to(state, select(receiver))
    }

    fn reserve_to<'pool>(
        &'pool self,
        state: &mut Directory<'storage, T>,
        receiver: &mut Option<Reservation<'pool, 'storage, T>>,
    ) -> Result<(), Error> {
        if receiver.is_some() {
            return Err(Error::Receiver);
        }
        let id = state.partial.or(state.empty).ok_or(Error::Full)?;
        let group = state.group(id);
        let index = group.head.expect("listed group has free slots");
        let slot = group.slot(index);
        // State is accessed only under the Pool Turn.
        let slot_state = unsafe { &slot.as_ref().state };
        let State::Free(next) = slot_state.get() else {
            unreachable!("the free chain contains only unclaimed slots");
        };
        let before = group.chain();
        group.head = next;
        group.free -= 1;
        slot_state.set(State::Reserved);
        state.free -= 1;
        state.reclassify(id, before);
        *receiver = Some(Reservation(Claim::new(self, id, index, slot)));
        Ok(())
    }
}

impl<'storage, T> Directory<'storage, T> {
    fn release(&mut self, id: GroupId, index: usize, expected: State) -> Result<(), Error> {
        let GroupRecord::Open(group) = self.entry(id)? else {
            return Err(Error::Stale);
        };
        let before = group.chain();
        // The Pool Turn keeps the open group and its slot mapped.
        let slot = unsafe { &group.slot(index).as_ref().state };
        if slot.get() != expected {
            return Err(Error::Stale);
        }
        slot.set(State::Free(group.head));
        group.head = Some(index);
        group.free += 1;
        self.free += 1;
        self.reclassify(id, before);
        Ok(())
    }

    fn entry(&mut self, id: GroupId) -> Result<&mut GroupRecord<'storage, T>, Error> {
        self.groups
            .get_mut(id.0)
            .and_then(|entry| entry.0.as_mut())
            .filter(|entry| entry.allocation() == id.1)
            .ok_or(Error::Stale)
    }

    fn group(&mut self, id: GroupId) -> &mut Group<'storage, T> {
        let GroupRecord::Open(group) = self.entry(id).expect("published group") else {
            unreachable!("only open groups have usable slots");
        };
        group
    }

    fn head(&mut self, chain: Chain) -> &mut Option<GroupId> {
        match chain {
            Chain::Empty => &mut self.empty,
            Chain::Partial => &mut self.partial,
        }
    }

    fn link(&mut self, id: GroupId, chain: Chain) {
        let next = self.head(chain).replace(id);
        if let Some(next) = next {
            self.group(next).links.previous = Some(id);
        }
        self.group(id).links = Links {
            previous: None,
            next,
        };
    }

    fn unlink(&mut self, id: GroupId, chain: Chain) {
        let Links { previous, next } = self.group(id).links;
        if let Some(previous) = previous {
            self.group(previous).links.next = next;
        } else {
            *self.head(chain) = next;
        }
        if let Some(next) = next {
            self.group(next).links.previous = previous;
        }
    }

    fn reclassify(&mut self, id: GroupId, before: Option<Chain>) {
        let after = self.group(id).chain();
        if before == after {
            return;
        }
        if let Some(chain) = before {
            self.unlink(id, chain);
        }
        if let Some(chain) = after {
            self.link(id, chain);
        }
    }
}
