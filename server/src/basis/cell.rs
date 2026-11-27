//! Physical ownership and a registered first access for an Internal table page.

use core::mem::MaybeUninit;

use limen_treaty::model::PhysicalRange;

use crate::metal::{
    identity::treaty::Generation,
    memory::{
        allocator::treaty::{AllocationId, MappedStorage, Retired},
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
    material: MappedStorage<'static>,
    owner: TableId,
    access: Option<Access>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Access {
    Prepared,
    Accepted,
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
        state.cell = Some(Record {
            material,
            owner,
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
            CellPermit { cell: id, owner },
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
    owner: TableId,
}

/// The one access record for this Cell generation and its original table node.
#[must_use]
pub(crate) struct CellAccess(CellPermit);

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
        if permit.cell != self.id || permit.owner != record.owner {
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
        record.access = Some(Access::Prepared);
        Ok(CellAccess(permit))
    }

    pub(crate) fn accept(
        &self,
        held: &mut CellTurn<'_>,
        permit: CellPermit,
        access: &CellAccess,
    ) -> Result<(), Error> {
        if access.0.cell != permit.cell || access.0.owner != permit.owner {
            return Err(Error::Scope);
        }
        let record = self.record(held, permit)?;
        if record.access != Some(Access::Prepared) {
            return Err(Error::Access);
        }
        record.access = Some(Access::Accepted);
        Ok(())
    }

    pub(crate) fn bytes<'held>(
        &self,
        held: &'held mut CellTurn<'_>,
        access: &CellAccess,
    ) -> Result<&'held mut [MaybeUninit<u8>], Error> {
        let record = self.record(held, access.0)?;
        if record.access != Some(Access::Accepted) {
            return Err(Error::Access);
        }
        Ok(record.material.bytes())
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
        if owner != record.owner || record.access.is_some() != access.is_some() {
            return Err(Error::Scope);
        }
        if access.is_some_and(|access| access.0.cell != self.id || access.0.owner != owner) {
            return Err(Error::Scope);
        }
        let (allocation, _) = state
            .cell
            .take()
            .expect("validated Cell")
            .material
            .into_parts();
        // No operation exposed this privately owned page to hardware, and
        // the original software borrow ended when its Cell was withdrawn.
        Ok(unsafe { allocation.retire() })
    }
}
