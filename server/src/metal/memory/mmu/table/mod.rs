//! Owned Internal table pages and their native branch relationships.

use core::{
    mem::{self, MaybeUninit},
    ptr,
};

use limen_treaty::model::{ByteLength, PhysicalAddress, PhysicalRange};

use crate::{
    basis::{
        cell::{self, Cell, CellAccess, CellId, CellPermit, CellSlot, CellTurn},
        pend::{self, Handle, Pend},
    },
    metal::{
        identity::treaty::Generation,
        memory::allocator::treaty::{AllocationId, InitialAccess, MappedStorage, Retired},
        sync::treaty::{TurnAcquireError, TurnLock, TurnMode, TurnWorkspace},
    },
};

use super::{
    aarch64, riscv64,
    treaty::{LeafAccess, Memory, Privilege},
    x86_64,
};

const ENTRIES: usize = 512;

mod branch;
pub use branch::TableBranch;

#[cfg(limen_target)]
mod local;
#[cfg(limen_target)]
pub use local::LocalRoot;
#[cfg(limen_cooperative)]
mod retirement;
#[cfg(limen_cooperative)]
pub use retirement::TableProgress;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TableFormat {
    Riscv64(riscv64::internal::format::Format),
    Aarch64(aarch64::internal::format::Format),
    X86_64(x86_64::internal::format::Format),
}

impl TableFormat {
    const fn levels(self) -> usize {
        match self {
            Self::Riscv64(format) => format.levels(),
            Self::Aarch64(_) | Self::X86_64(_) => 4,
        }
    }

    fn branch(self, address: PhysicalAddress) -> Option<u64> {
        match self {
            Self::Riscv64(_) => riscv64::internal::format::Format::branch(address),
            Self::Aarch64(format) => format.branch(address),
            Self::X86_64(format) => format.branch(address, Privilege::Kernel),
        }
    }

    fn matches_branch(self, address: PhysicalAddress, observed: u64) -> bool {
        match self {
            Self::X86_64(format) => format.matches_branch(address, observed),
            _ => self.branch(address) == Some(observed),
        }
    }

    fn leaf(
        self,
        address: PhysicalAddress,
        level: usize,
        access: LeafAccess,
        memory: Memory,
    ) -> Option<u64> {
        match self {
            Self::Riscv64(format) if memory == Memory::Normal => {
                format.leaf(address, level, access)
            }
            Self::Aarch64(format) => format.leaf(address, level, Privilege::Kernel, access, memory),
            Self::X86_64(format) => format.leaf(address, level, Privilege::Kernel, access, memory),
            _ => None,
        }
    }

    fn accepts(self, material: &MappedStorage<'_>) -> bool {
        // All three admitted formats use one 4 KiB table of 64-bit entries.
        let range = material.physical();
        if range.length() != ByteLength::new(4096)
            || !material.mapped_address().is_multiple_of(4096)
        {
            return false;
        }
        self.branch(range.start()).is_some()
    }
}

/// Stable scope and generation of the original table page.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TableId {
    store: usize,
    generation: Generation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TableError {
    Acquire(TurnAcquireError),
    Pend(pend::Error),
    Cell(cell::Error),
    Switch(super::treaty::SwitchError),
    Return(crate::metal::memory::allocator::treaty::Error),
    Layout,
    Scope,
    State,
    Occupied,
    Exhausted,
}

impl From<TurnAcquireError> for TableError {
    fn from(error: TurnAcquireError) -> Self {
        Self::Acquire(error)
    }
}

impl From<pend::Error> for TableError {
    fn from(error: pend::Error) -> Self {
        Self::Pend(error)
    }
}

impl From<cell::Error> for TableError {
    fn from(error: cell::Error) -> Self {
        Self::Cell(error)
    }
}

struct Construction {
    cell: Cell,
    holding: Pend<CellPermit>,
    access: Option<CellAccess>,
}

struct TablePage {
    cell: Cell,
    access: CellAccess,
    initialized: bool,
    exposure: Exposure,
    children: usize,
}

impl TablePage {
    fn entry<'held>(
        &self,
        held: &'held mut CellTurn<'_>,
        index: usize,
    ) -> Result<&'held mut [MaybeUninit<u8>; 8], TableError> {
        let bytes = self.cell.bytes(held, &self.access)?;
        let (entries, []) = bytes.as_chunks_mut::<8>() else {
            unreachable!("native table geometry");
        };
        entries.get_mut(index).ok_or(TableError::Layout)
    }

    fn read(raw: &[MaybeUninit<u8>; 8]) -> u64 {
        // The private node is initialized before it grants descriptor access.
        u64::from_le_bytes(raw.map(|byte| unsafe { byte.assume_init() }))
    }

    fn write(raw: &mut [MaybeUninit<u8>; 8], value: u64) {
        *raw = value.to_le_bytes().map(MaybeUninit::new);
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Exposure {
    Private,
    Child {
        parent: TableId,
        index: usize,
    },
    #[cfg(limen_target)]
    Local {
        borrowed: Option<local::BorrowedBranch>,
    },
    #[cfg(limen_cooperative)]
    Retiring(TableId),
}

enum Node {
    Vacant,
    Constructing(Construction),
    Admitted(TablePage),
    #[cfg(limen_cooperative)]
    Returning {
        root: TableId,
        allocation: AllocationId,
    },
}

impl Node {
    fn cell(&self) -> Result<&Cell, TableError> {
        match self {
            Self::Vacant => Err(TableError::State),
            Self::Constructing(construction) => Ok(&construction.cell),
            Self::Admitted(page) => Ok(&page.cell),
            #[cfg(limen_cooperative)]
            Self::Returning { .. } => Err(TableError::State),
        }
    }

    fn private(&mut self) -> Result<&mut TablePage, TableError> {
        match self {
            Self::Admitted(page) if page.initialized && page.exposure == Exposure::Private => {
                Ok(page)
            }
            _ => Err(TableError::State),
        }
    }

    fn entry<'held>(
        &mut self,
        held: &'held mut CellTurn<'_>,
        index: usize,
    ) -> Result<&'held mut [MaybeUninit<u8>; 8], TableError> {
        self.private()?.entry(held, index)
    }
}

struct State {
    generation: Option<Generation>,
    node: Node,
    branches: Option<&'static mut [TableBranch]>,
}

/// Permanent metadata for one owned Internal table page at its native level.
pub struct TableStore {
    format: TableFormat,
    level: usize,
    state: TurnLock<State>,
}

impl TableStore {
    pub const fn new(format: TableFormat) -> Self {
        Self::at_level(format, format.levels() - 1, None)
    }

    /// Supply permanent metadata for the given level and child capacity.
    pub fn for_level(
        format: TableFormat,
        level: usize,
        branches: &'static mut [TableBranch],
    ) -> Option<Self> {
        if level >= format.levels()
            || branches.len() > ENTRIES
            || (level == 0 && !branches.is_empty())
            || !branches.iter().all(TableBranch::is_empty)
        {
            return None;
        }
        Some(Self::at_level(format, level, Some(branches)))
    }

    const fn at_level(
        format: TableFormat,
        level: usize,
        branches: Option<&'static mut [TableBranch]>,
    ) -> Self {
        Self {
            format,
            level,
            state: TurnLock::new(State {
                generation: None,
                node: Node::Vacant,
                branches,
            }),
        }
    }

    /// Publish the Cell and its one internal Pend under the original live
    /// issuing holding. Failure returns the exact unpublished material.
    pub fn publish_cell(
        &'static self,
        cell: &'static CellSlot,
        issuer: Handle,
        material: MappedStorage<'static>,
        mode: TurnMode,
    ) -> Result<TableId, (MappedStorage<'static>, TableError)> {
        if !self.format.accepts(&material) {
            return Err((material, TableError::Layout));
        }
        let mut original = Some(material);
        let mut work = TurnWorkspace::<4>::new();
        let mut plan = work.plan();
        if let Err(error) = issuer.plan_initial_access(&mut plan) {
            return Err((original.take().expect("original material"), error.into()));
        }
        cell.plan(&mut plan);
        plan.insert(&self.state).expect("original TableStore");
        let committed = plan.acquire(mode, |held| {
            let InitialAccess::ReadWrite =
                issuer.initial_access(held, original.as_ref().expect("unpublished material"))?;
            let state = held.access(&self.state).expect("original TableStore");
            if !matches!(&state.node, Node::Vacant) {
                return Err(TableError::Occupied);
            }
            let generation = Generation::next(state.generation).ok_or(TableError::Exhausted)?;
            let id = TableId {
                store: ptr::from_ref(self).addr(),
                generation,
            };
            let (cell, permit) = cell.publish(held, id, &mut original)?;
            let state = held.access(&self.state).expect("original TableStore");
            state.generation = Some(generation);
            state.node = Node::Constructing(Construction {
                cell,
                holding: Pend::held(permit),
                access: None,
            });
            Ok(id)
        });
        committed
            .map_err(TableError::from)
            .and_then(core::convert::identity)
            .map_err(|error| {
                (
                    original.expect("failed publication preserves material"),
                    error,
                )
            })
    }

    fn check(&self, state: &State, id: TableId) -> Result<(), TableError> {
        if id.store != ptr::from_ref(self).addr() || state.generation != Some(id.generation) {
            return Err(TableError::Scope);
        }
        Ok(())
    }

    fn with_node<R>(
        &self,
        id: TableId,
        mode: TurnMode,
        apply: impl FnOnce(&mut Node, &mut CellTurn<'_>) -> Result<R, TableError>,
    ) -> Result<R, TableError> {
        let cell = self.cell_slot(id)?;
        let mut work = TurnWorkspace::<2>::new();
        let mut plan = work.plan();
        plan.insert(&self.state).expect("original TableStore");
        cell.plan(&mut plan);
        plan.acquire(mode, |held| {
            let (state, mut cell_locked) = cell.together(held, &self.state);
            self.check(state, id)?;
            apply(&mut state.node, &mut cell_locked)
        })?
    }

    pub fn prepare(&self, id: TableId, mode: TurnMode) -> Result<(), TableError> {
        self.with_node(id, mode, |node, held| {
            let Node::Constructing(construction) = node else {
                return Err(TableError::State);
            };
            let permit = construction.holding.resolve()?;
            if construction.access.is_none() {
                construction.access = Some(construction.cell.prepare(held, permit)?);
            }
            Ok(())
        })
    }

    /// Retain the original node, transfer the access and exhaust the unique
    /// construction holding as one commit.
    pub fn accept(&self, id: TableId, mode: TurnMode) -> Result<(), TableError> {
        self.with_node(id, mode, |node, held| {
            let Node::Constructing(construction) = node else {
                return if matches!(node, Node::Admitted(_)) {
                    Ok(())
                } else {
                    Err(TableError::State)
                };
            };
            let permit = construction.holding.resolve()?;
            let access = construction.access.as_ref().ok_or(TableError::State)?;
            construction.cell.accept(held, permit, access)?;
            construction.holding.consume().expect("validated holding");
            let Node::Constructing(construction) = mem::replace(node, Node::Vacant) else {
                unreachable!("validated construction");
            };
            *node = Node::Admitted(TablePage {
                cell: construction.cell,
                access: construction.access.expect("accepted access"),
                initialized: false,
                exposure: Exposure::Private,
                children: 0,
            });
            Ok(())
        })
    }

    /// Close the exact construction branch without ending its retained duties.
    pub fn shut(&self, id: TableId) -> Result<(), TableError> {
        self.state.acquire(|state| {
            self.check(state, id)?;
            let Node::Constructing(construction) = &mut state.node else {
                return Err(TableError::State);
            };
            construction.holding.shut();
            Ok(())
        })?
    }

    pub fn cell(&self, id: TableId) -> Result<(CellId, PhysicalRange), TableError> {
        self.state.acquire(|state| {
            self.check(state, id)?;
            let cell = state.node.cell()?;
            Ok((cell.id(), cell.physical()))
        })?
    }

    /// Recover the original node decision from the exact issued allocation.
    /// A later generation at the same address is a separate material.
    pub fn locate(&self, allocation: AllocationId) -> Result<Option<TableId>, TableError> {
        Ok(self.state.acquire(|state| {
            #[cfg(limen_cooperative)]
            let retained = if let Node::Returning { allocation, .. } = &state.node {
                *allocation
            } else {
                state.node.cell().ok()?.allocation()
            };
            #[cfg(not(limen_cooperative))]
            let retained = state.node.cell().ok()?.allocation();
            (retained == allocation).then(|| TableId {
                store: ptr::from_ref(self).addr(),
                generation: state.generation.expect("live node generation"),
            })
        })?)
    }

    /// Initialize an original private node to the native invalid descriptors.
    pub fn initialize(&self, id: TableId, mode: TurnMode) -> Result<(), TableError> {
        self.with_node(id, mode, |node, held| {
            let Node::Admitted(page) = node else {
                return Err(TableError::State);
            };
            if page.exposure != Exposure::Private {
                return Err(TableError::State);
            }
            if !page.initialized {
                page.cell
                    .bytes(held, &page.access)?
                    .fill(mem::MaybeUninit::new(0));
                page.initialized = true;
            }
            Ok(())
        })
    }

    pub fn descriptor(&self, id: TableId, index: usize, mode: TurnMode) -> Result<u64, TableError> {
        self.with_node(id, mode, |node, held| {
            Ok(TablePage::read(node.entry(held, index)?))
        })
    }

    /// Construct a native supervisor-only leaf in this private table.
    /// The actual format must support the requested memory attributes.
    ///
    /// # Safety
    /// The caller must own or retain the actual mapped material and its
    /// matching attributes and rights through all subsequent native uses of
    /// the tree. The leaf cannot manufacture that authority from an address.
    pub unsafe fn map_leaf(
        &self,
        id: TableId,
        index: usize,
        physical: PhysicalAddress,
        access: LeafAccess,
        memory: Memory,
        mode: TurnMode,
    ) -> Result<(), TableError> {
        let leaf = self
            .format
            .leaf(physical, self.level, access, memory)
            .ok_or(TableError::Layout)?;
        self.with_node(id, mode, |node, held| {
            let entry = node.entry(held, index)?;
            if TablePage::read(entry) != 0 {
                return Err(TableError::Occupied);
            }
            TablePage::write(entry, leaf);
            Ok(())
        })
    }

    fn cell_slot(&self, id: TableId) -> Result<&'static CellSlot, TableError> {
        self.state.acquire(|state| {
            self.check(state, id)?;
            state.node.cell().map(Cell::slot)
        })?
    }

    /// End this node's private access and transfer the exact material's
    /// retirement to a preallocated receiver. The allocator still owns refund.
    ///
    /// # Safety
    /// The receiver belongs to the durable owner of the original return.
    pub unsafe fn retire_private(
        &self,
        id: TableId,
        receiver: &mut Option<Retired>,
        mode: TurnMode,
    ) -> Result<(), TableError> {
        if receiver.is_some() {
            return Err(TableError::Occupied);
        }
        self.with_node(id, mode, |node, held| {
            let (cell, access) = match node {
                Node::Constructing(construction) => {
                    (&construction.cell, construction.access.as_ref())
                }
                Node::Admitted(page)
                    if page.exposure == Exposure::Private && page.children == 0 =>
                {
                    (&page.cell, Some(&page.access))
                }
                Node::Admitted(_) => return Err(TableError::State),
                Node::Vacant => return Err(TableError::State),
                #[cfg(limen_cooperative)]
                Node::Returning { .. } => return Err(TableError::State),
            };
            *receiver = Some(cell.retire_private(held, id, access)?);
            *node = Node::Vacant;
            Ok(())
        })
    }
}
