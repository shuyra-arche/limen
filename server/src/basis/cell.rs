//! Physical ownership and exact access registration.
//! Ordinary RAM construction and dedicated Internal table pages share the
//! same physical instance identity and source ownership.

use core::mem::MaybeUninit;

use limen_treaty::model::{ByteOffset, ByteRange, PhysicalRange};

use crate::metal::{
    identity::treaty::{Generation, Key, Slot},
    memory::{
        access::treaty::ram::{MappedRange, RetainedRange},
        allocator::treaty::{
            Allocation, AllocationId, InitialAccess, MappedStorage, RamPurpose, Retired,
        },
        mmu::treaty::TableId,
    },
    sync::treaty::{TurnAccess, TurnAcquireError, TurnLock, TurnMode, TurnPlan, TurnWorkspace},
};

use super::pend::{self, Handle, Pend};

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
    role: Role,
    access: Option<AccessRecord>,
}

/// The dedicated first-use policy is not a different physical source.
#[derive(Clone, Copy, Eq, PartialEq)]
enum Role {
    Ordinary,
    Table(TableId),
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

/// One stable physical instance position. Dedicated table access has one
/// inline record; ordinary multi-access admission is not yet enabled.
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
        self.publish_for(held, Role::Table(owner), material)
    }

    fn publish_for(
        &'static self,
        held: &mut TurnAccess<'_>,
        role: Role,
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
            storage: match role {
                Role::Ordinary => Storage::Retained(mapping.into_retained()),
                Role::Table(_) => Storage::Exclusive(mapping),
            },
            role,
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
                role,
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
    Pend(pend::Error),
    Acquire(TurnAcquireError),
}

impl From<pend::Error> for Error {
    fn from(error: pend::Error) -> Self {
        Self::Pend(error)
    }
}

impl From<TurnAcquireError> for Error {
    fn from(error: TurnAcquireError) -> Self {
        Self::Acquire(error)
    }
}

/// The sole disposition right for the original physical instance.
#[must_use]
pub(crate) struct Cell {
    slot: &'static CellSlot,
    id: CellId,
    allocation: AllocationId,
    physical: PhysicalRange,
}

/// The first Read/Write ceiling covers this exact instance and range.
/// Its role restricts dedicated table access to the original table node.
#[derive(Clone, Copy)]
pub(crate) struct CellPermit {
    cell: CellId,
    role: Role,
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
            || permit.role != record.role
            || !matches!(record.role, Role::Table(_))
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
        if record.role != Role::Table(owner) || record.access.is_some() != access.is_some() {
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

struct Construction {
    cell: Cell,
    holding: Pend<CellPermit>,
}

/// Permanent receiving position for an ordinary RAM construction. The
/// trusted Kernel receiver retains the disposition right and the actual
/// first direct Pend holding; it is not a user PendSpace or a new Cell type.
pub struct CellConstruction(TurnLock<Option<Construction>>);

impl CellConstruction {
    pub const fn new() -> Self {
        Self(TurnLock::new(None))
    }

    /// Publish the original cleared RAM and an independent first Read/Write
    /// holding. The caller owns the original unpublished material until this
    /// commit. Failure returns that material, possibly already cleared.
    pub fn publish(
        &'static self,
        slot: &'static CellSlot,
        issuer: Handle,
        mut material: MappedStorage<'static>,
        mode: TurnMode,
    ) -> Result<CellId, (MappedStorage<'static>, Error)> {
        // Clearing private bytes may be expensive. Never hold a Turn while
        // doing it, and do not retain an exclusive byte borrow after publication.
        material.clear();
        let mut original = Some(material);
        let mut workspace = TurnWorkspace::<4>::new();
        let mut plan = workspace.plan();
        if let Err(error) = issuer.plan_initial_access(&mut plan) {
            return Err((original.expect("original material"), error.into()));
        }
        slot.plan(&mut plan);
        plan.insert(&self.0).expect("original construction");
        let committed = plan.acquire(mode, |held| {
            let InitialAccess::ReadWrite = issuer.initial_access(
                held,
                original.as_ref().expect("unpublished material"),
                RamPurpose::Cell,
            )?;
            if held.access(&self.0).expect("original construction").is_some() {
                return Err(Error::Occupied);
            }
            let (cell, permit) = slot.publish_for(held, Role::Ordinary, &mut original)?;
            let id = cell.id();
            *held.access(&self.0).expect("original construction") = Some(Construction {
                cell,
                holding: Pend::held(permit),
            });
            Ok(id)
        });
        committed
            .map_err(Error::from)
            .and_then(core::convert::identity)
            .map_err(|error| (original.expect("unpublished material"), error))
    }

    /// Recover the original publication by its exact Allocation identity.
    pub fn locate(&self, allocation: AllocationId) -> Result<Option<CellId>, Error> {
        Ok(self.0.acquire(|state| {
            state.as_ref().and_then(|record| {
                (record.cell.allocation() == allocation).then_some(record.cell.id())
            })
        })?)
    }

    fn original(
        state: &mut Option<Construction>,
        id: CellId,
    ) -> Result<&mut Construction, Error> {
        state
            .as_mut()
            .filter(|record| record.cell.id() == id)
            .ok_or(Error::Scope)
    }

    /// Observe the direct holding's current ceiling, not access authority
    /// that can be reused after dropping this protection.
    pub fn initial_range(&self, id: CellId) -> Result<ByteRange, Error> {
        self.0.acquire(|state| {
            let record = Self::original(state, id)?;
            let permit = record.holding.resolve()?;
            Ok(permit.range)
        })?
    }

    /// The trusted construction receiver closes its own first branch,
    /// independently of the original Allocation spending branch.
    pub fn revoke(&self, id: CellId) -> Result<(), Error> {
        self.0.acquire(|state| {
            Self::original(state, id)?.holding.shut();
            Ok(())
        })?
    }

    /// Settle the direct operation holding without releasing disposition.
    pub fn release(&self, id: CellId) -> Result<(), Error> {
        self.0.acquire(|state| {
            Self::original(state, id)?.holding.consume()?;
            Ok(())
        })?
    }

    /// Return a construction whose ordinary access has never been admitted.
    /// The real receiver takes the exact original Allocation retirement;
    /// refund remains an independent Allocator operation.
    ///
    /// # Safety
    /// The receiver belongs to the actual durable return owner. Preserve
    /// the backing storage and original mapping until this transfer.
    pub unsafe fn retire(
        &self,
        id: CellId,
        receiver: &mut Option<Retired>,
        mode: TurnMode,
    ) -> Result<(), Error> {
        if receiver.is_some() {
            return Err(Error::Occupied);
        }
        let slot = self
            .0
            .acquire(|state| Ok::<_, Error>(Self::original(state, id)?.cell.slot()))??;
        let mut workspace = TurnWorkspace::<2>::new();
        let mut plan = workspace.plan();
        plan.insert(&self.0).expect("original construction");
        slot.plan(&mut plan);
        plan.acquire(mode, |held| {
            let (construction, mut cell_locked) = slot.together(held, &self.0);
            let original = Self::original(construction, id)?;
            let cell = original.cell.state(&mut cell_locked)?;
            let record = cell.cell.as_ref().expect("live Cell");
            if record.role != Role::Ordinary
                || record.access.is_some()
                || !matches!(record.storage, Storage::Retained(_))
            {
                return Err(Error::Access);
            }
            let Record {
                allocation,
                storage,
                ..
            } = cell.cell.take().expect("validated Cell");
            let Storage::Retained(_ended) = storage else {
                unreachable!("validated retained storage");
            };
            // No ordinary access path is enabled yet. This exact original
            // construction owns the only disposition and the direct holding.
            original.holding.shut();
            match original.holding.consume() {
                Ok(_) | Err(pend::Error::Exhausted) => {}
                Err(_) => unreachable!("direct holding consumption"),
            }
            *receiver = Some(unsafe { allocation.retire() });
            *construction = None;
            Ok(())
        })?
    }
}

impl Default for CellConstruction {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[path = "../../tests/basis/cell.rs"]
mod tests;
