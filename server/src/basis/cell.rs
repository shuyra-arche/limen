//! Physical ownership and exact access registration.
//! The first implemented access policy is a dedicated Internal table page.

use core::mem::MaybeUninit;

use limen_treaty::model::{ByteOffset, ByteRange, PhysicalRange};

use crate::metal::{
    identity::treaty::{Generation, Key, Slot},
    memory::{
        access::treaty::ram::{MappedRange, RetainedRange},
        allocator::treaty::{Allocation, AllocationId, MappedStorage, Retired},
        mmu::treaty::TableId,
    },
    sync::treaty::{TurnAccess, TurnLock, TurnPlan},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CellId {
    slot: usize,
    generation: Generation,
}

struct State {
    generation: Option<Generation>,
    cell: Option<Record>,
}

struct Record {
    allocation: Allocation,
    storage: Storage,
    table: TableId,
    access: Option<AccessRecord>,
}

enum Storage {
    Exclusive(MappedRange<'static>),
    Retained(RetainedRange<'static>),
}

/// Identifies one registered use in an exact Cell instance, not a permit
/// or the identity of the owner that consumes the use.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct AccessId(Key<CellId>);

#[derive(Clone, Copy, Eq, PartialEq)]
enum AccessPhase {
    Prepared,
    Accepted,
}

struct AccessRecord {
    id: AccessId,
    range: ByteRange,
    phase: AccessPhase,
}

impl AccessRecord {
    /// A dedicated table Cell admits exactly one full-range read/write use.
    /// Its slot is never recycled within the life of that Cell.
    fn prepare(cell: CellId, range: ByteRange) -> (Self, CellAccess) {
        let id = AccessId(Key::new(
            cell,
            Slot::new(0).expect("dedicated access slot"),
            Generation::FIRST,
        ));
        (
            Self {
                id,
                range,
                phase: AccessPhase::Prepared,
            },
            CellAccess { id },
        )
    }

    fn check(&self, access: &CellAccess) -> Result<(), Error> {
        if access.id != self.id {
            return Err(Error::Scope);
        }
        Ok(())
    }

    fn accept(&mut self, access: &CellAccess, range: ByteRange) -> Result<(), Error> {
        self.check(access)?;
        if self.range != range {
            return Err(Error::Scope);
        }
        if self.phase != AccessPhase::Prepared {
            return Err(Error::Access);
        }
        self.phase = AccessPhase::Accepted;
        Ok(())
    }

    fn accepted(&self, access: &CellAccess) -> Result<(), Error> {
        self.check(access)?;
        if self.phase != AccessPhase::Accepted {
            return Err(Error::Access);
        }
        Ok(())
    }
}

impl Record {
    fn access(&self, access: &CellAccess) -> Result<&AccessRecord, Error> {
        let original = self.access.as_ref().ok_or(Error::Access)?;
        original.check(access)?;
        Ok(original)
    }

    fn accepted(&self, access: &CellAccess) -> Result<(), Error> {
        self.access(access)?.accepted(access)
    }
}

fn full_range(physical: PhysicalRange) -> ByteRange {
    ByteRange::new(ByteOffset::new(0), ByteOffset::new(physical.length().get()))
        .expect("nonempty physical range")
}

/// One stable Cell position, with prepaid space for its single table access.
pub struct CellSlot(TurnLock<State>);

pub(crate) struct CellTurn<'held> {
    slot: &'held CellSlot,
    state: &'held mut State,
}

impl CellSlot {
    pub const fn new() -> Self {
        Self(TurnLock::new(State {
            generation: None,
            cell: None,
        }))
    }

    pub(crate) fn plan<'a>(&'a self, plan: &mut TurnPlan<'a, '_>) {
        plan.insert(&self.0).expect("Cell storage");
    }

    pub(crate) fn together<'held, Owner>(
        &'held self,
        held: &'held mut TurnAccess<'_>,
        owner: &'held TurnLock<Owner>,
    ) -> (&'held mut Owner, CellTurn<'held>) {
        let (owner, state) = held
            .access_pair(owner, &self.0)
            .expect("original owner and Cell");
        (owner, CellTurn { slot: self, state })
    }

    pub(crate) fn publish(
        &'static self,
        held: &mut TurnAccess<'_>,
        owner: TableId,
        material: &mut Option<MappedStorage<'static>>,
    ) -> Result<(Cell, CellPermit), Error> {
        let scope = core::ptr::from_ref(self).addr();
        let state = held.access(&self.0).expect("Cell storage");
        if state.cell.is_some() {
            return Err(Error::Occupied);
        }
        let generation = Generation::next(state.generation).ok_or(Error::Exhausted)?;
        let material = material.take().expect("original issued material");
        let allocation = material.id();
        let physical = material.physical();
        let (issued, mapping) = material.into_parts();
        state.cell = Some(Record {
            allocation: issued,
            storage: Storage::Exclusive(mapping),
            table: owner,
            access: None,
        });
        state.generation = Some(generation);
        let id = CellId {
            slot: scope,
            generation,
        };
        Ok((
            Cell {
                slot: self,
                id,
                allocation,
                physical,
            },
            CellPermit {
                cell: id,
                table: owner,
                range: full_range(physical),
            },
        ))
    }
}

impl Default for CellSlot {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Occupied,
    Exhausted,
    Scope,
    Access,
}

/// The sole disposition right for the original physical instance.
#[must_use]
pub(crate) struct Cell {
    slot: &'static CellSlot,
    id: CellId,
    allocation: AllocationId,
    physical: PhysicalRange,
}

/// The first access is read/write across the exact Cell for this one table node.
#[derive(Clone, Copy)]
pub(crate) struct CellPermit {
    cell: CellId,
    table: TableId,
    range: ByteRange,
}

/// Linear evidence of the registered use, independent of its issuing permit.
#[must_use]
pub(crate) struct CellAccess {
    id: AccessId,
}

impl Cell {
    pub(crate) fn slot(&self) -> &'static CellSlot {
        self.slot
    }

    pub(crate) fn id(&self) -> CellId {
        self.id
    }

    pub(crate) fn allocation(&self) -> AllocationId {
        self.allocation
    }

    pub(crate) fn physical(&self) -> PhysicalRange {
        self.physical
    }

    fn state<'held>(&self, held: &'held mut CellTurn<'_>) -> Result<&'held mut State, Error> {
        let state = &mut *held.state;
        if !core::ptr::eq(self.slot, held.slot)
            || state.generation != Some(self.id.generation)
            || state.cell.is_none()
        {
            return Err(Error::Scope);
        }
        Ok(state)
    }

    fn record<'held>(
        &self,
        held: &'held mut CellTurn<'_>,
        permit: CellPermit,
    ) -> Result<&'held mut Record, Error> {
        let record = self.state(held)?.cell.as_mut().expect("live Cell");
        if permit.cell != self.id
            || permit.table != record.table
            || permit.range != full_range(self.physical)
        {
            return Err(Error::Scope);
        }
        Ok(record)
    }

    pub(crate) fn prepare(
        &self,
        held: &mut CellTurn<'_>,
        permit: CellPermit,
    ) -> Result<CellAccess, Error> {
        let record = self.record(held, permit)?;
        if record.access.is_some() {
            return Err(Error::Access);
        }
        let (entry, access) = AccessRecord::prepare(self.id, permit.range);
        record.access = Some(entry);
        Ok(access)
    }

    pub(crate) fn accept(
        &self,
        held: &mut CellTurn<'_>,
        permit: CellPermit,
        access: &CellAccess,
    ) -> Result<(), Error> {
        let record = self.record(held, permit)?;
        record
            .access
            .as_mut()
            .ok_or(Error::Access)?
            .accept(access, permit.range)
    }

    pub(crate) fn bytes<'held>(
        &self,
        held: &'held mut CellTurn<'_>,
        access: &CellAccess,
    ) -> Result<&'held mut [MaybeUninit<u8>], Error> {
        let record = self.state(held)?.cell.as_mut().expect("live Cell");
        record.accepted(access)?;
        let range = record.access(access)?.range;
        let Storage::Exclusive(mapping) = &mut record.storage else {
            return Err(Error::Access);
        };
        let start = usize::try_from(range.start().get()).map_err(|_| Error::Scope)?;
        let end = usize::try_from(range.end().get()).map_err(|_| Error::Scope)?;
        mapping.bytes().get_mut(start..end).ok_or(Error::Scope)
    }

    /// Retain the exact storage while ending its exclusive byte reference.
    pub(crate) fn freeze(&self, held: &mut CellTurn<'_>, access: &CellAccess) -> Result<(), Error> {
        let record = self.state(held)?.cell.as_mut().expect("live Cell");
        record.accepted(access)?;
        if !matches!(record.storage, Storage::Exclusive(_)) {
            return Err(Error::Access);
        }
        let record = held.state.cell.take().expect("checked Cell access");
        let Storage::Exclusive(mapping) = record.storage else {
            unreachable!("checked exclusive mapping");
        };
        held.state.cell = Some(Record {
            storage: Storage::Retained(mapping.into_retained()),
            ..record
        });
        Ok(())
    }

    /// # Safety
    /// The original owner has excluded all hardware and conflicting access to
    /// this page and kept its original working mapping live.
    pub(crate) unsafe fn thaw(
        &self,
        held: &mut CellTurn<'_>,
        access: &CellAccess,
    ) -> Result<(), Error> {
        let record = self.state(held)?.cell.as_mut().expect("live Cell");
        record.accepted(access)?;
        if !matches!(record.storage, Storage::Retained(_)) {
            return Err(Error::Access);
        }
        let record = held.state.cell.take().expect("checked Cell access");
        let Storage::Retained(mapping) = record.storage else {
            unreachable!("checked retained mapping");
        };
        held.state.cell = Some(Record {
            storage: Storage::Exclusive(unsafe { mapping.into_exclusive() }),
            ..record
        });
        Ok(())
    }

    /// Withdraw the original table's only private use and end its mapped borrow.
    ///
    /// The actual table owner has excluded publication to hardware and retains
    /// the TableStore Turn. Prepared and accepted private uses both end here.
    pub(crate) fn retire_private(
        &self,
        held: &mut CellTurn<'_>,
        owner: TableId,
        access: Option<&CellAccess>,
    ) -> Result<Retired, Error> {
        let state = self.state(held)?;
        let record = state.cell.as_ref().expect("live Cell");
        if owner != record.table || record.access.is_some() != access.is_some() {
            return Err(Error::Scope);
        }
        if let Some(access) = access {
            record.access(access)?;
        }
        if !matches!(record.storage, Storage::Exclusive(_)) {
            return Err(Error::Access);
        }
        let Record {
            allocation,
            storage,
            ..
        } = state.cell.take().expect("validated Cell");
        let Storage::Exclusive(_ended) = storage else {
            unreachable!("checked exclusive mapping");
        };
        // Hardware reachability has ended and the former mapping is no longer used.
        Ok(unsafe { allocation.retire() })
    }
}

#[cfg(test)]
#[path = "../../tests/basis/cell.rs"]
mod tests;
