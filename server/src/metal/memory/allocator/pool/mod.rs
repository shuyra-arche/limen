//! Fixed, mapped storage for a consumer's typed objects.

mod group;
mod slot;

use crate::metal::sync::{TurnAccess, TurnAcquireError, TurnLock, TurnPlan, TurnPlanError};

use super::{AllocationId, Error as SourceError, MappedStorage};
use group::{Chain, Group, Links};

use group::GroupRecord;
pub use group::{Geometry, Retirement, Supply};
use slot::{Claim, Completion, SlotPosition, State};
pub use slot::{
    DestructionReceipt, Occupied, Prepared, ReleaseCursor, ReleaseProgress, Reservation,
};
pub use slot::{Detached, REFERENCE_LIMIT, Reclamation, Reference, Stable, StableOwner};

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

impl From<TurnAcquireError> for Error {
    fn from(error: TurnAcquireError) -> Self {
        Self::Source(error.into())
    }
}

/// A fixed directory position qualified by its original Allocation.
/// The exact source AllocationId distinguishes every later group at this position.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GroupId(usize, AllocationId);

/// Storage supplied by the actual consumer outside its reclaimable pages.
pub struct GroupSlot<'storage, T> {
    record: Option<GroupRecord<'storage, T>>,
    next: Option<usize>,
}

impl<T> GroupSlot<'_, T> {
    pub const EMPTY: Self = Self {
        record: None,
        next: None,
    };
}

/// Unique history acknowledgement authority for the exact installed group.
#[must_use]
pub struct GroupReceipt {
    id: GroupId,
}

impl GroupReceipt {
    pub fn id(&self) -> GroupId {
        self.id
    }

    /// End the retained result only after the original refund has settled.
    pub fn acknowledge<T>(&self, pool: &Pool<'_, T>) -> Result<(), Error> {
        pool.state.acquire(|state| {
            if !matches!(state.entry(self.id)?, GroupRecord::Retired(_)) {
                return Err(Error::Busy);
            }
            let slot = &mut state.groups[self.id.0];
            slot.record = None;
            slot.next = state.free_group;
            state.free_group = Some(self.id.0);
            Ok(())
        })?
    }
}

pub struct Pool<'storage, T> {
    state: TurnLock<Directory<'storage, T>>,
}

struct Directory<'storage, T> {
    groups: &'storage mut [GroupSlot<'storage, T>],
    extent: usize,
    free_group: Option<usize>,
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
    /// Establish the fixed group directory outside all supplied group mappings.
    pub fn new(groups: &'storage mut [GroupSlot<'storage, T>]) -> Result<Self, Error> {
        if groups.is_empty()
            || groups
                .iter()
                .any(|entry| entry.record.is_some() || entry.next.is_some())
        {
            return Err(Error::Directory);
        }
        let mut free_group = None;
        for (index, slot) in groups.iter_mut().enumerate().rev() {
            slot.next = free_group;
            free_group = Some(index);
        }
        Ok(Self {
            state: TurnLock::new(Directory {
                groups,
                extent: 0,
                free_group,
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

    /// Accept already acquired mapped material. The caller keeps the exact
    /// pair during private initialization; only final publication consumes it.
    /// A failure retains the pair and original receipt position.
    ///
    /// # Safety
    /// The original owner retains the pair and its mapping/exclusivity through
    /// preparation, every use and retirement. The receipt position belongs to
    /// the actual consumer and remains durable until history acknowledgement.
    /// The Pool directory and root stay outside every supplied group.
    pub unsafe fn install(
        &self,
        material: &mut Option<MappedStorage<'storage>>,
        receipt: &mut Option<GroupReceipt>,
    ) -> Result<GroupId, Error> {
        if receipt.is_some() {
            return Err(Error::Receiver);
        }
        let supplied = material.as_ref().ok_or(Error::Material)?;
        let mapping = supplied.mapping();
        let geometry = Geometry::of::<T>(mapping.length())?;
        if !mapping.address().is_multiple_of(geometry.alignment)
            || !supplied
                .physical()
                .start()
                .get()
                .is_multiple_of(geometry.alignment as u64)
        {
            return Err(Error::Layout);
        }
        let retirement = supplied.allocation().return_position()?;
        self.state
            .acquire(|state| state.free_group.ok_or(Error::Directory).map(|_| ()))??;

        // No Pool protection is held during the full private slot walk.
        Group::<T>::prepare(
            geometry,
            material
                .as_mut()
                .expect("checked material")
                .mapping_mut()
                .bytes(),
        );
        self.state.acquire(|state| {
            let index = state.free_group.ok_or(Error::Directory)?;
            let capacity = state
                .capacity
                .checked_add(geometry.slots)
                .ok_or(Error::Directory)?;
            let bytes = state
                .bytes
                .checked_add(geometry.bytes)
                .ok_or(Error::Directory)?;
            let (allocation, mapping) = material.take().expect("checked material").into_parts();
            let id = GroupId(index, allocation.id());
            let group = Group::new(allocation, retirement, geometry, mapping.into_bytes());
            let slot = &mut state.groups[index];
            assert!(slot.record.is_none());
            state.free_group = slot.next.take();
            slot.record = Some(GroupRecord::Open(group));
            state.extent = state.extent.max(index + 1);
            state.open += 1;
            state.capacity = capacity;
            state.free += geometry.slots;
            state.bytes = bytes;
            state.link(id, Chain::Empty);
            *receipt = Some(GroupReceipt { id });
            Ok(id)
        })?
    }

    pub fn capacity(&self) -> Result<Capacity, Error> {
        Ok(self.state.acquire(|state| Capacity {
            groups: state.open,
            bytes: state.bytes,
            slots: state.capacity,
            free: state.free,
        })?)
    }

    pub fn supply(&self, id: GroupId) -> Result<Supply, Error> {
        self.state.acquire(|state| match state.entry(id)? {
            GroupRecord::Open(group) => Ok(group.supply()),
            _ => Err(Error::Stale),
        })?
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
        *receiver = Some(Reservation(Claim::new(
            self,
            SlotPosition { group: id, index },
            slot,
        )));
        Ok(())
    }
}

impl<'storage, T> Directory<'storage, T> {
    fn release(&mut self, position: SlotPosition, expected: State) -> Result<(), Error> {
        let SlotPosition { group: id, index } = position;
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
            .and_then(|entry| entry.record.as_mut())
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
