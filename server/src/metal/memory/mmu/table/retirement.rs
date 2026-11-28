//! Bounded retirement of the original unreachable owned tree.
//! Detached branches keep their exact child until its return is acknowledged.
//! Only the exclusive LocalRoot owner advances this one outstanding refund.

use super::{Exposure, Node, State, TableError, TableId, TablePage, TableStore, branch::Branch};
use crate::metal::{
    memory::allocator::treaty::{AllocationId, Retired, ReturnReceipt},
    sync::treaty::{TurnMode, TurnWorkspace},
};
use limen_treaty::model::PhysicalRange;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TableProgress {
    Pending,
    Complete,
}

#[derive(Clone, Copy)]
struct Position {
    store: &'static TableStore,
    id: TableId,
}

#[derive(Clone, Copy)]
struct Parent {
    position: Position,
    branch: Branch,
}

#[derive(Clone, Copy)]
enum Phase {
    Material,
    Returning,
    Refunded,
    Acknowledged,
}

struct Pending {
    position: Position,
    parent: Option<Parent>,
    allocation: AllocationId,
    physical: PhysicalRange,
    material: Option<Retired>,
    receipt: Option<ReturnReceipt>,
    phase: Phase,
}

pub(super) struct Retirement {
    root: Position,
    pending: Option<Pending>,
}

impl Retirement {
    pub(super) const fn new(store: &'static TableStore, id: TableId) -> Self {
        Self {
            root: Position { store, id },
            pending: None,
        }
    }

    /// One bounded step, with no Turn held across an Allocator call.
    /// The original local owner has already restored the native context
    /// and closed the root to editing and native reinstallation.
    pub(super) fn advance(&mut self, mode: TurnMode) -> Result<TableProgress, TableError> {
        let root = self.root.id;
        if let Some(pending) = self.pending.as_mut() {
            match pending.phase {
                Phase::Material => {
                    let material = pending.material.take().expect("original retired material");
                    if let Err((material, error)) =
                        unsafe { material.begin_return(&mut pending.receipt) }
                    {
                        pending.material = Some(material);
                        return Err(TableError::Return(error));
                    }
                    pending.phase = Phase::Returning;
                }
                Phase::Returning => {
                    let returned = pending.receipt.as_ref().expect("original receipt")
                        .finish(mode).map_err(TableError::Return)?;
                    if returned.allocation != pending.allocation || returned.physical != pending.physical {
                        return Err(TableError::Scope);
                    }
                    pending.phase = Phase::Refunded;
                }
                Phase::Refunded => {
                    pending.receipt.as_ref().expect("completed receipt")
                        .acknowledge().map_err(TableError::Return)?;
                    pending.receipt = None;
                    pending.phase = Phase::Acknowledged;
                }
                Phase::Acknowledged => {
                    pending.release(root, mode)?;
                    let complete = pending.position.id == root;
                    self.pending = None;
                    if complete {
                        return Ok(TableProgress::Complete);
                    }
                }
            }
            return Ok(TableProgress::Pending);
        }

        // Follow the actual retained branch records, not a second directory.
        // Every descent reduces the native level; the maximum is Sv57's
        // five levels. A detached child is still discoverable from the root.
        let mut position = self.root;
        let mut parent = None;
        for _ in 0..self.root.store.format.levels() {
            let branch = position.first(root)?;
            let Some(branch) = branch else {
                position.return_page(root, parent, &mut self.pending, mode)?;
                return Ok(TableProgress::Pending);
            };
            if !branch.detached {
                position.detach(root, branch, mode)?;
                return Ok(TableProgress::Pending);
            }
            if position.store.level != branch.child.level + 1 {
                return Err(TableError::State);
            }
            parent = Some(Parent { position, branch });
            position = Position { store: branch.child, id: branch.id };
        }
        Err(TableError::State)
    }
}

fn retiring(node: &mut Node, root: TableId) -> Result<&mut TablePage, TableError> {
    match node {
        Node::Admitted(page) if page.exposure == Exposure::Retiring(root) => Ok(page),
        _ => Err(TableError::State),
    }
}

impl Position {
    fn first(self, root: TableId) -> Result<Option<Branch>, TableError> {
        self.store.state.acquire(|state| {
            self.store.check(state, self.id)?;
            let children = retiring(&mut state.node, root)?.children;
            let first = state.first_branch();
            if (children == 0) != first.is_none() {
                return Err(TableError::State);
            }
            Ok(first)
        })?
    }

    fn detach(self, root: TableId, branch: Branch, mode: TurnMode) -> Result<(), TableError> {
        let child = branch.child;
        if self.store.level != child.level + 1 || self.store.format != child.format {
            return Err(TableError::Layout);
        }
        self.store.with_branch(self.id, branch, mode, |held, parent_slot, child_slot| {
            let (parent, mut cell) = parent_slot.together(held, &self.store.state);
            let (index, original) = parent.branch(branch.index).ok_or(TableError::State)?;
            if original.id != branch.id || original.detached {
                return Err(TableError::Scope);
            }
            let page = retiring(&mut parent.node, root)?;
            let encoded = TablePage::read(page.entry(&mut cell, branch.index)?);

            let (child_state, mut cell) = child_slot.together(held, &child.state);
            let Node::Admitted(page) = &mut child_state.node else {
                return Err(TableError::State);
            };
            if page.exposure != (Exposure::Child { parent: self.id, index: branch.index })
                || !self.store.format.matches_branch(page.cell.physical().start(), encoded)
            {
                return Err(TableError::State);
            }
            // This parent is already unreachable by native hardware and
            // cannot be republished. Thaw the exact child into retirement,
            // never into ordinary private access.
            unsafe { page.cell.thaw(&mut cell, &page.access) }?;
            page.exposure = Exposure::Retiring(root);

            let (parent, mut cell) = parent_slot.together(held, &self.store.state);
            let page = retiring(&mut parent.node, root).expect("original retiring parent");
            TablePage::write(page.entry(&mut cell, branch.index).expect("original entry"), 0);
            // Keep both the child locator and its counted obligation.
            parent.set_branch(index, Some(Branch { detached: true, ..branch }));
            Ok(())
        })
    }

    fn return_page(
        self,
        root: TableId,
        parent: Option<Parent>,
        pending: &mut Option<Pending>,
        mode: TurnMode,
    ) -> Result<(), TableError> {
        if pending.is_some() {
            return Err(TableError::Occupied);
        }
        self.store.with_node(self.id, mode, |node, held| {
            let page = retiring(node, root)?;
            if page.children != 0 {
                return Err(TableError::State);
            }
            let allocation = page.cell.allocation();
            let physical = page.cell.physical();
            let material = page.cell.retire_private(held, self.id, Some(&page.access))?;
            *node = Node::Returning { root, allocation };
            // Publish the exact material into its prepaid receiver while
            // the original table state is still protected.
            *pending = Some(Pending {
                position: self,
                parent,
                allocation,
                physical,
                material: Some(material),
                receipt: None,
                phase: Phase::Material,
            });
            Ok(())
        })
    }
}

impl Pending {
    fn check(&self, root: TableId, state: &State) -> Result<(), TableError> {
        self.position.store.check(state, self.position.id)?;
        if !matches!(state.node, Node::Returning { root: owner, allocation }
            if owner == root && allocation == self.allocation)
            || state.first_branch().is_some()
        {
            return Err(TableError::State);
        }
        Ok(())
    }

    /// Release the original node only after the source acknowledged the
    /// exact receipt. A failed acquisition leaves this Acknowledged phase
    /// intact, so retry cannot repeat the refund or its acknowledgement.
    fn release(&self, root: TableId, mode: TurnMode) -> Result<(), TableError> {
        let store = self.position.store;
        let Some(Parent { position: parent, branch }) = self.parent else {
            if self.position.id != root {
                return Err(TableError::Scope);
            }
            return store.state.acquire(|state| {
                self.check(root, state)?;
                state.node = Node::Vacant;
                Ok(())
            })?;
        };
        let mut workspace = TurnWorkspace::<2>::new();
        let mut plan = workspace.plan();
        plan.insert(&parent.store.state).expect("parent retirement");
        plan.insert(&store.state).expect("original return node");
        plan.acquire(mode, |held| {
            let (parent_state, child_state) = held.access_pair(&parent.store.state, &store.state)
                .expect("original parent and child");
            self.check(root, child_state)?;
            parent.store.check(parent_state, parent.id)?;
            let (index, retained) = parent_state.branch(branch.index).ok_or(TableError::State)?;
            if retained.id != self.position.id || !retained.detached || !core::ptr::eq(retained.child, store) {
                return Err(TableError::Scope);
            }
            let page = retiring(&mut parent_state.node, root)?;
            page.children = page.children.checked_sub(1).ok_or(TableError::State)?;
            parent_state.set_branch(index, None);
            child_state.node = Node::Vacant;
            Ok(())
        })?
    }
}
