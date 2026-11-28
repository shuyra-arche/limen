//! Physical ownership and exact access registration.
//! Ordinary RAM construction and dedicated Internal table pages share the
//! same physical instance identity and source ownership.

use core::mem::MaybeUninit;

use limen_treaty::model::{ByteOffset, ByteRange, PhysicalRange};

use crate::metal::{
    identity::treaty::{Entry, Generation, GenerationTable, Key, ReserveError, Slot},
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
    accesses: AccessStore,
}

struct Record {
    allocation: Allocation,
    storage: Storage,
    role: Role,
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
pub struct AccessId(Key<CellId>);

/// Physical byte access rights; the original RAM grant currently admits R/W.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Rights(u8);

impl Rights {
    pub const READ: Self = Self(1);
    pub const WRITE: Self = Self(2);
    pub const EXECUTE: Self = Self(4);
    pub const READ_WRITE: Self = Self(3);

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    fn covers(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AccessOwner {
    Table(TableId),
    Preparation(usize),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AccessPhase {
    Prepared,
    Accepted,
}

#[derive(Debug)]
struct AccessRecord {
    id: AccessId,
    range: ByteRange,
    rights: Rights,
    owner: AccessOwner,
    phase: AccessPhase,
}

impl AccessRecord {
    fn prepare(
        id: AccessId,
        range: ByteRange,
        rights: Rights,
        owner: AccessOwner,
    ) -> (Self, CellAccess) {
        (
            Self {
                id,
                range,
                rights,
                owner,
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

    fn prepared(&self, access: &CellAccess, owner: AccessOwner) -> Result<(), Error> {
        self.check(access)?;
        if self.owner != owner {
            return Err(Error::Scope);
        }
        if self.phase != AccessPhase::Prepared {
            return Err(Error::Access);
        }
        Ok(())
    }

    fn accept(
        &mut self,
        access: &CellAccess,
        range: ByteRange,
        rights: Rights,
        owner: AccessOwner,
    ) -> Result<(), Error> {
        self.prepared(access, owner)?;
        if self.range != range || self.rights != rights {
            return Err(Error::Scope);
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

/// Explicitly supplied ordinary Cell access metadata, not data-page storage.
/// It stays with the stable Cell position across instance reuse.
pub struct AccessStorage<const CAPACITY: usize> {
    entries: [Entry<AccessRecord>; CAPACITY],
}

impl<const CAPACITY: usize> AccessStorage<CAPACITY> {
    pub const fn new() -> Self {
        Self {
            entries: [const { Entry::EMPTY }; CAPACITY],
        }
    }
}

impl<const CAPACITY: usize> Default for AccessStorage<CAPACITY> {
    fn default() -> Self {
        Self::new()
    }
}

enum AccessStore {
    Table(Option<AccessRecord>),
    Ordinary(OrdinaryAccesses),
}

struct OrdinaryAccesses {
    scope: usize,
    entries: GenerationTable<'static, usize, AccessRecord>,
    active: usize,
}

impl OrdinaryAccesses {
    fn new<const CAPACITY: usize>(
        storage: &'static mut AccessStorage<CAPACITY>,
    ) -> Result<Self, Error> {
        if CAPACITY == 0 {
            return Err(Error::Capacity);
        }
        let scope = core::ptr::from_ref(storage).addr();
        let entries =
            GenerationTable::new(scope, &mut storage.entries).map_err(|_| Error::Capacity)?;
        Ok(Self {
            scope,
            entries,
            active: 0,
        })
    }

    fn prepare(
        &mut self,
        cell: CellId,
        range: ByteRange,
        rights: Rights,
        owner: AccessOwner,
    ) -> Result<CellAccess, Error> {
        let reservation = self.entries.reserve().map_err(|error| match error {
            ReserveError::IdentityExhausted => Error::Exhausted,
            _ => Error::Capacity,
        })?;
        let key = reservation.key();
        let id = AccessId(Key::new(cell, key.slot(), key.generation()));
        let (record, access) = AccessRecord::prepare(id, range, rights, owner);
        self.entries
            .publish(reservation, record)
            .expect("original reserved access position");
        self.active += 1;
        Ok(access)
    }

    fn cancel(&mut self, access: &CellAccess, owner: AccessOwner) -> Result<(), Error> {
        let id = access.id.0;
        let key = Key::new(self.scope, id.slot(), id.generation());
        self.entries
            .get(key)
            .ok_or(Error::Scope)?
            .prepared(access, owner)?;
        self.entries.detach(key).expect("validated access");
        let (reclamation, _) = self.entries.reclaim(key).expect("detached access");
        self.entries
            .release(reclamation)
            .expect("settled access position");
        self.active -= 1;
        Ok(())
    }
}

impl AccessStore {
    fn is_empty(&self) -> bool {
        match self {
            Self::Table(record) => record.is_none(),
            Self::Ordinary(store) => store.active == 0,
        }
    }

    fn table(&self) -> Result<&Option<AccessRecord>, Error> {
        match self {
            Self::Table(record) => Ok(record),
            Self::Ordinary(_) => Err(Error::Scope),
        }
    }

    fn table_mut(&mut self) -> Result<&mut Option<AccessRecord>, Error> {
        match self {
            Self::Table(record) => Ok(record),
            Self::Ordinary(_) => Err(Error::Scope),
        }
    }

    fn ordinary(&mut self) -> Result<&mut OrdinaryAccesses, Error> {
        match self {
            Self::Ordinary(store) => Ok(store),
            Self::Table(_) => Err(Error::Scope),
        }
    }

    fn table_accepted(&self, access: &CellAccess) -> Result<&AccessRecord, Error> {
        let record = self.table()?.as_ref().ok_or(Error::Access)?;
        record.accepted(access)?;
        Ok(record)
    }
}

fn full_range(physical: PhysicalRange) -> ByteRange {
    ByteRange::new(ByteOffset::new(0), ByteOffset::new(physical.length().get()))
        .expect("nonempty physical range")
}

/// One stable physical instance position. Dedicated table access has one
/// inline record; ordinary Cell access capacity is supplied explicitly.
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
            accesses: AccessStore::Table(None),
        }))
    }

    pub fn for_cells<const CAPACITY: usize>(
        storage: &'static mut AccessStorage<CAPACITY>,
    ) -> Result<Self, Error> {
        Ok(Self(TurnLock::new(State {
            generation: None,
            cell: None,
            accesses: AccessStore::Ordinary(OrdinaryAccesses::new(storage)?),
        })))
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
        if !matches!(
            (&state.accesses, role),
            (AccessStore::Table(_), Role::Table(_)) | (AccessStore::Ordinary(_), Role::Ordinary)
        ) || !state.accesses.is_empty()
        {
            return Err(Error::Scope);
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
                rights: Rights::READ_WRITE,
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
    Capacity,
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
    rights: Rights,
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

    fn table_permit<'held>(
        &self,
        held: &'held mut CellTurn<'_>,
        permit: CellPermit,
    ) -> Result<(&'held mut Option<AccessRecord>, TableId), Error> {
        let state = self.state(held)?;
        let record = state.cell.as_ref().expect("live Cell");
        let Role::Table(table) = record.role else {
            return Err(Error::Scope);
        };
        if permit.cell != self.id
            || permit.role != record.role
            || permit.range != full_range(self.physical)
            || permit.rights != Rights::READ_WRITE
        {
            return Err(Error::Scope);
        }
        Ok((state.accesses.table_mut()?, table))
    }

    pub(crate) fn prepare(
        &self,
        held: &mut CellTurn<'_>,
        permit: CellPermit,
    ) -> Result<CellAccess, Error> {
        let (record, table) = self.table_permit(held, permit)?;
        if record.is_some() {
            return Err(Error::Access);
        }
        // The dedicated table slot is never recycled within this Cell.
        let id = AccessId(Key::new(
            self.id,
            Slot::new(0).expect("dedicated access slot"),
            Generation::FIRST,
        ));
        let (entry, access) =
            AccessRecord::prepare(id, permit.range, permit.rights, AccessOwner::Table(table));
        *record = Some(entry);
        Ok(access)
    }

    pub(crate) fn accept(
        &self,
        held: &mut CellTurn<'_>,
        permit: CellPermit,
        access: &CellAccess,
    ) -> Result<(), Error> {
        let (record, table) = self.table_permit(held, permit)?;
        record.as_mut().ok_or(Error::Access)?.accept(
            access,
            permit.range,
            permit.rights,
            AccessOwner::Table(table),
        )
    }

    pub(crate) fn bytes<'held>(
        &self,
        held: &'held mut CellTurn<'_>,
        access: &CellAccess,
    ) -> Result<&'held mut [MaybeUninit<u8>], Error> {
        let state = self.state(held)?;
        let range = state.accesses.table_accepted(access)?.range;
        let record = state.cell.as_mut().expect("live Cell");
        let Storage::Exclusive(mapping) = &mut record.storage else {
            return Err(Error::Access);
        };
        let start = usize::try_from(range.start().get()).map_err(|_| Error::Scope)?;
        let end = usize::try_from(range.end().get()).map_err(|_| Error::Scope)?;
        mapping.bytes().get_mut(start..end).ok_or(Error::Scope)
    }

    /// Locate one descriptor of this exact accepted, retained table use.
    /// The caller must keep the TableStore and Cell protected during the
    /// actual native access. The pointer does not grant a Rust byte borrow.
    #[cfg(limen_cooperative)]
    pub(crate) fn retained_word(
        &self,
        held: &mut CellTurn<'_>,
        access: &CellAccess,
        index: usize,
    ) -> Result<core::ptr::NonNull<u64>, Error> {
        let state = self.state(held)?;
        let registered = state.accesses.table_accepted(access)?;
        if registered.range != full_range(self.physical)
            || registered.rights != Rights::READ_WRITE
        {
            return Err(Error::Scope);
        }
        let record = state.cell.as_ref().expect("live Cell");
        let Storage::Retained(storage) = &record.storage else {
            return Err(Error::Access);
        };
        storage.word(index).ok_or(Error::Scope)
    }

    /// Retain the exact storage while ending its exclusive byte reference.
    pub(crate) fn freeze(&self, held: &mut CellTurn<'_>, access: &CellAccess) -> Result<(), Error> {
        let state = self.state(held)?;
        state.accesses.table_accepted(access)?;
        let record = state.cell.as_ref().expect("live Cell");
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
        let state = self.state(held)?;
        state.accesses.table_accepted(access)?;
        let record = state.cell.as_ref().expect("live Cell");
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
        let registered = state.accesses.table()?;
        if record.role != Role::Table(owner) || registered.is_some() != access.is_some() {
            return Err(Error::Scope);
        }
        if let Some(access) = access {
            registered.as_ref().expect("registered use").check(access)?;
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
        *state.accesses.table_mut().expect("validated table storage") = None;
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
            if held
                .access(&self.0)
                .expect("original construction")
                .is_some()
            {
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

    fn original(state: &mut Option<Construction>, id: CellId) -> Result<&mut Construction, Error> {
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

    /// Register one physical byte access in the original supplied receiver.
    /// No hardware or ordinary byte access is authorized by preparation.
    pub fn prepare(
        &self,
        id: CellId,
        range: ByteRange,
        rights: Rights,
        receiver: &'static AccessPreparation,
        mode: TurnMode,
    ) -> Result<AccessId, Error> {
        let slot = self
            .0
            .acquire(|state| Ok::<_, Error>(Self::original(state, id)?.cell.slot()))??;
        let mut workspace = TurnWorkspace::<3>::new();
        let mut plan = workspace.plan();
        plan.insert(&self.0).expect("original construction");
        slot.plan(&mut plan);
        plan.insert(&receiver.0).expect("original access receiver");
        plan.acquire(mode, |held| {
            if held
                .access(&receiver.0)
                .expect("original access receiver")
                .is_some()
            {
                return Err(Error::Occupied);
            }
            let (construction, mut locked) = slot.together(held, &self.0);
            let original = Self::original(construction, id)?;
            let permit = original.holding.resolve()?;
            if permit.cell != id
                || permit.role != Role::Ordinary
                || !permit.range.covers(range)
                || !permit.rights.covers(rights)
            {
                return Err(Error::Scope);
            }
            let state = original.cell.state(&mut locked)?;
            let record = state.cell.as_ref().expect("live Cell");
            if record.role != Role::Ordinary || !matches!(record.storage, Storage::Retained(_)) {
                return Err(Error::Access);
            }
            let access = state
                .accesses
                .ordinary()?
                .prepare(id, range, rights, receiver.owner())?;
            let id = access.id;
            *held.access(&receiver.0).expect("original access receiver") =
                Some(Preparation { slot, access });
            Ok(id)
        })?
    }

    /// Return a construction whose ordinary access has never been accepted.
    /// A prepared access still occupies its original record until cancellation.
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
                || !cell.accesses.is_empty()
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
            // Every admitted preparation must be settled before transferring
            // the original disposition to the return owner.
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

struct Preparation {
    slot: &'static CellSlot,
    access: CellAccess,
}

/// Actual durable receiving position for one ordinary prepared access. The
/// original Cell and this receiver jointly publish and cancel its linear
/// credential. An AccessId returned to an observer is not that credential.
pub struct AccessPreparation(TurnLock<Option<Preparation>>);

impl AccessPreparation {
    pub const fn new() -> Self {
        Self(TurnLock::new(None))
    }

    fn owner(&self) -> AccessOwner {
        AccessOwner::Preparation(core::ptr::from_ref(self).addr())
    }

    pub fn observe(&self) -> Result<Option<AccessId>, Error> {
        Ok(self
            .0
            .acquire(|state| state.as_ref().map(|p| p.access.id))?)
    }

    /// Cancel only the exact unaccepted preparation. Revocation and release
    /// of its source holding cannot invalidate this settlement path.
    pub fn cancel(&self, id: AccessId, mode: TurnMode) -> Result<(), Error> {
        let slot = self.0.acquire(|state| {
            state
                .as_ref()
                .filter(|p| p.access.id == id)
                .map(|p| p.slot)
                .ok_or(Error::Scope)
        })??;
        let mut workspace = TurnWorkspace::<2>::new();
        let mut plan = workspace.plan();
        plan.insert(&self.0).expect("original access receiver");
        slot.plan(&mut plan);
        plan.acquire(mode, |held| {
            let (preparation, locked) = slot.together(held, &self.0);
            let original = preparation
                .as_ref()
                .filter(|p| p.access.id == id)
                .ok_or(Error::Scope)?;
            let cell = id.0.scope();
            let state = locked.state;
            if !core::ptr::eq(slot, original.slot)
                || cell.slot != core::ptr::from_ref(slot).addr()
                || state.generation != Some(cell.generation)
                || state.cell.is_none()
            {
                return Err(Error::Scope);
            }
            state
                .accesses
                .ordinary()?
                .cancel(&original.access, self.owner())?;
            *preparation = None;
            Ok(())
        })?
    }
}

impl Default for AccessPreparation {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[path = "../../tests/basis/cell.rs"]
mod tests;
