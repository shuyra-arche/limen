//! A branch retains the exact initialized page that its descriptor reaches.

use super::{ENTRIES, Exposure, Node, State, TableError, TableId, TablePage, TableStore};
use crate::{
    basis::cell::CellSlot,
    metal::sync::treaty::{TurnAccess, TurnMode, TurnWorkspace},
};

#[derive(Clone, Copy)]
pub(super) struct Branch {
    pub(super) index: usize,
    pub(super) child: &'static TableStore,
    pub(super) id: TableId,
    #[cfg(limen_cooperative)]
    pub(super) detached: bool,
}

/// Prepaid custody for one actual child, held by its original parent.
pub struct TableBranch(Option<Branch>);

impl TableBranch {
    pub const EMPTY: Self = Self(None);

    pub(super) fn is_empty(&self) -> bool {
        self.0.is_none()
    }
}

impl State {
    #[cfg(limen_cooperative)]
    pub(super) fn first_branch(&self) -> Option<Branch> {
        self.branches.as_deref()?.iter().find_map(|slot| slot.0)
    }

    pub(super) fn branch(&self, index: usize) -> Option<(usize, Branch)> {
        self.branches
            .as_deref()?
            .iter()
            .enumerate()
            .find_map(|(position, slot)| {
                slot.0
                    .filter(|branch| branch.index == index)
                    .map(|branch| (position, branch))
            })
    }

    pub(super) fn free_branch(&self) -> Option<usize> {
        self.branches
            .as_deref()?
            .iter()
            .position(|slot| slot.0.is_none())
    }

    pub(super) fn set_branch(&mut self, position: usize, branch: Option<Branch>) {
        self.branches
            .as_deref_mut()
            .expect("prepaid branch custody")[position]
            .0 = branch;
    }
}

impl TableStore {
    pub(super) fn with_branch<R>(
        &self,
        id: TableId,
        branch: Branch,
        mode: TurnMode,
        apply: impl FnOnce(&mut TurnAccess<'_>, &CellSlot, &CellSlot) -> Result<R, TableError>,
    ) -> Result<R, TableError> {
        let Branch {
            child,
            id: child_id,
            ..
        } = branch;
        let parent_slot = self.cell_slot(id)?;
        let child_slot = child.cell_slot(child_id)?;
        let mut work = TurnWorkspace::<4>::new();
        let mut plan = work.plan();
        plan.insert(&self.state).expect("parent metadata");
        plan.insert(&child.state).expect("child metadata");
        parent_slot.plan(&mut plan);
        child_slot.plan(&mut plan);
        plan.acquire(mode, |held| {
            self.check(held.access(&self.state).expect("parent metadata"), id)?;
            child.check(held.access(&child.state).expect("child metadata"), child_id)?;
            apply(held, parent_slot, child_slot)
        })?
    }

    /// Publish a native branch and retain the exact initialized child together.
    pub fn link_child(
        &self,
        id: TableId,
        index: usize,
        child: &'static TableStore,
        child_id: TableId,
        mode: TurnMode,
    ) -> Result<(), TableError> {
        if index >= ENTRIES || self.level != child.level + 1 || self.format != child.format {
            return Err(TableError::Layout);
        }
        let branch = Branch {
            index,
            child,
            id: child_id,
            #[cfg(limen_cooperative)]
            detached: false,
        };
        self.with_branch(id, branch, mode, |held, parent_slot, child_slot| {
            let (parent, mut cell) = parent_slot.together(held, &self.state);
            if TablePage::read(parent.node.entry(&mut cell, index)?) != 0 {
                return Err(TableError::Occupied);
            }
            let position = parent.free_branch().ok_or(TableError::Exhausted)?;
            let (stored_child, mut cell) = child_slot.together(held, &child.state);
            let page = stored_child.node.private()?;
            let encoded = self
                .format
                .branch(page.cell.physical().start())
                .ok_or(TableError::Layout)?;
            page.cell.freeze(&mut cell, &page.access)?;
            page.exposure = Exposure::Child { parent: id, index };

            let (parent, mut cell) = parent_slot.together(held, &self.state);
            let page = parent.node.private().expect("validated private parent");
            TablePage::write(
                page.entry(&mut cell, index)
                    .expect("validated branch entry"),
                encoded,
            );
            page.children += 1;
            parent.set_branch(position, Some(branch));
            Ok(())
        })
    }

    /// Remove a branch from an unreachable parent and reopen its exact child.
    pub fn unlink_child(
        &self,
        id: TableId,
        index: usize,
        mode: TurnMode,
    ) -> Result<TableId, TableError> {
        let branch = self.state.acquire(|state| {
            self.check(state, id)?;
            state
                .branch(index)
                .map(|(_, branch)| branch)
                .ok_or(TableError::State)
        })??;
        let child = branch.child;
        self.with_branch(id, branch, mode, |held, parent_slot, child_slot| {
            let (parent, mut cell) = parent_slot.together(held, &self.state);
            let (position, retained) = parent.branch(index).ok_or(TableError::State)?;
            if retained.id != branch.id {
                return Err(TableError::Scope);
            }
            // Access to these bytes requires that the original parent is offline.
            let encoded = TablePage::read(parent.node.entry(&mut cell, index)?);
            let (stored_child, mut cell) = child_slot.together(held, &child.state);
            let Node::Admitted(page) = &mut stored_child.node else {
                return Err(TableError::State);
            };
            if page.exposure != (Exposure::Child { parent: id, index })
                || !self
                    .format
                    .matches_branch(page.cell.physical().start(), encoded)
            {
                return Err(TableError::State);
            }
            // Both owners are locked. The inaccessible parent is the only
            // possible hardware path to the child, and cannot be installed.
            unsafe { page.cell.thaw(&mut cell, &page.access) }?;
            page.exposure = Exposure::Private;
            let (parent, mut cell) = parent_slot.together(held, &self.state);
            let page = parent.node.private().expect("validated private parent");
            TablePage::write(
                page.entry(&mut cell, index)
                    .expect("validated branch entry"),
                0,
            );
            page.children -= 1;
            parent.set_branch(position, None);
            Ok(branch.id)
        })
    }
}
