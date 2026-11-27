//! The first owned root of an Internal table tree.

use core::{mem, ptr};

use limen_treaty::model::{ByteLength, PhysicalRange};

use crate::{
    basis::{
        cell::{self, Cell, CellAccess, CellId, CellPermit, CellSlot, CellTurn},
        pend::{self, Handle, Pend},
    },
    metal::{
        identity::treaty::Generation,
        memory::allocator::treaty::{InitialAccess, MappedStorage, Retired},
        sync::treaty::{TurnAcquireError, TurnLock, TurnMode, TurnWorkspace},
    },
};

use super::{aarch64, riscv64, treaty::Privilege, x86_64};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TableFormat {
    Riscv64(riscv64::internal::format::Format),
    Aarch64(aarch64::internal::format::Format),
    X86_64(x86_64::internal::format::Format),
}

impl TableFormat {
    fn accepts(self, material: &MappedStorage<'_>) -> bool {
        // All three admitted formats use one 4 KiB table of 64-bit entries.
        let range = material.physical();
        if range.length() != ByteLength::new(4096)
            || !material.mapped_address().is_multiple_of(4096)
        {
            return false;
        }
        match self {
            Self::Riscv64(_) => riscv64::internal::format::Format::branch(range.start()),
            Self::Aarch64(format) => format.branch(range.start()),
            Self::X86_64(format) => format.branch(range.start(), Privilege::Kernel),
        }
        .is_some()
    }
}

/// Stable scope and generation of the original root node.
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
}

enum Root {
    Vacant,
    Constructing(Construction),
    Private(TablePage),
}

impl Root {
    fn cell(&self) -> Result<&Cell, TableError> {
        match self {
            Self::Vacant => Err(TableError::State),
            Self::Constructing(construction) => Ok(&construction.cell),
            Self::Private(page) => Ok(&page.cell),
        }
    }
}

struct State {
    generation: Option<Generation>,
    root: Root,
}

/// Permanent storage and exact scope of one owned Internal root. The root
/// remains unreachable to hardware throughout its ordinary construction.
pub struct TableStore {
    format: TableFormat,
    state: TurnLock<State>,
}

impl TableStore {
    pub const fn new(format: TableFormat) -> Self {
        Self {
            format,
            state: TurnLock::new(State {
                generation: None,
                root: Root::Vacant,
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
            if !matches!(&state.root, Root::Vacant) {
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
            state.root = Root::Constructing(Construction {
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

    fn with_root<R>(
        &self,
        id: TableId,
        mode: TurnMode,
        apply: impl FnOnce(&mut Root, &mut CellTurn<'_>) -> Result<R, TableError>,
    ) -> Result<R, TableError> {
        let cell = self.state.acquire(|state| {
            self.check(state, id)?;
            state.root.cell().map(Cell::slot)
        })??;
        let mut work = TurnWorkspace::<2>::new();
        let mut plan = work.plan();
        plan.insert(&self.state).expect("original TableStore");
        cell.plan(&mut plan);
        plan.acquire(mode, |held| {
            let (state, mut cell_locked) = cell.together(held, &self.state);
            self.check(state, id)?;
            apply(&mut state.root, &mut cell_locked)
        })?
    }

    pub fn prepare(&self, id: TableId, mode: TurnMode) -> Result<(), TableError> {
        self.with_root(id, mode, |root, held| {
            let Root::Constructing(construction) = root else {
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
        self.with_root(id, mode, |root, held| {
            let Root::Constructing(construction) = root else {
                return if matches!(root, Root::Private(_)) {
                    Ok(())
                } else {
                    Err(TableError::State)
                };
            };
            let permit = construction.holding.resolve()?;
            let access = construction.access.as_ref().ok_or(TableError::State)?;
            construction.cell.accept(held, permit, access)?;
            construction.holding.consume().expect("validated holding");
            let Root::Constructing(construction) = mem::replace(root, Root::Vacant) else {
                unreachable!("validated construction");
            };
            *root = Root::Private(TablePage {
                cell: construction.cell,
                access: construction.access.expect("accepted access"),
                initialized: false,
            });
            Ok(())
        })
    }

    /// Close the exact construction branch without ending its retained duties.
    pub fn shut(&self, id: TableId) -> Result<(), TableError> {
        self.state.acquire(|state| {
            self.check(state, id)?;
            let Root::Constructing(construction) = &mut state.root else {
                return Err(TableError::State);
            };
            construction.holding.shut();
            Ok(())
        })?
    }

    pub fn cell(&self, id: TableId) -> Result<(CellId, PhysicalRange), TableError> {
        self.state.acquire(|state| {
            self.check(state, id)?;
            let cell = state.root.cell()?;
            Ok((cell.id(), cell.physical()))
        })?
    }

    /// Initialize an original private root to the native invalid descriptors.
    pub fn initialize(&self, id: TableId, mode: TurnMode) -> Result<(), TableError> {
        self.with_root(id, mode, |root, held| {
            let Root::Private(page) = root else {
                return Err(TableError::State);
            };
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
        self.with_root(id, mode, |root, held| {
            let Root::Private(page) = root else {
                return Err(TableError::State);
            };
            if !page.initialized {
                return Err(TableError::State);
            }
            let bytes = page.cell.bytes(held, &page.access)?;
            let (descriptors, []) = bytes.as_chunks::<8>() else {
                unreachable!("native table geometry");
            };
            let raw = descriptors.get(index).ok_or(TableError::Layout)?;
            // The original exclusive root has finished initialization.
            Ok(u64::from_le_bytes(
                raw.map(|byte| unsafe { byte.assume_init() }),
            ))
        })
    }

    /// End this root's private access and transfer the exact material's
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
        self.with_root(id, mode, |root, held| {
            let (cell, access) = match root {
                Root::Constructing(construction) => {
                    (&construction.cell, construction.access.as_ref())
                }
                Root::Private(page) => (&page.cell, Some(&page.access)),
                Root::Vacant => return Err(TableError::State),
            };
            *receiver = Some(cell.retire_private(held, id, access)?);
            *root = Root::Vacant;
            Ok(())
        })
    }
}
