//! Transfer a complete private empty-leaf path into the installed tree.

use limen_treaty::model::{InternalAddress, PhysicalAddress};

use super::{
    LocalRoot, TableError, TableId, TableStore,
    path::{Path, Position, coordinate},
};
use crate::basis::cell::CellTurn;
use crate::metal::{
    memory::mmu::table::{ENTRIES, Exposure, State, TablePage, branch::Branch},
    sync::treaty::TurnMode,
};

/// The original root retains an accepted non-leaf publication until its
/// actual synchronization and readback. The parent owns the child even
/// if this step cannot finish immediately.
#[derive(Clone, Copy)]
pub(super) struct Pending {
    parent: Position,
    branch: Branch,
    physical: PhysicalAddress,
}

// The actual private chain is traversed before its top is linked to the
// installed parent. Descendants are already retained by their exact private
// parent; none of these snapshots transfers a Cell or TablePage.
#[derive(Clone, Copy)]
struct Expected {
    branch: Branch,
    descriptor: u64,
}

fn check_children(state: &State, expected: Option<Expected>) -> Result<(), TableError> {
    match (state.first_branch(), expected) {
        (None, None) => Ok(()),
        (Some(original), Some(expected))
            if !original.detached
                && original.index == expected.branch.index
                && original.id == expected.branch.id
                && core::ptr::eq(original.child, expected.branch.child) =>
        {
            Ok(())
        }
        _ => Err(TableError::Scope),
    }
}

fn check_page(
    page: &TablePage,
    held: &mut CellTurn<'_>,
    expected: Option<Expected>,
    private: bool,
) -> Result<(), TableError> {
    if page.children != usize::from(expected.is_some())
        || page.claims.iter().any(|&bits| bits != 0)
    {
        return Err(TableError::Occupied);
    }
    let descriptor = |index| {
        expected
            .filter(|entry| entry.branch.index == index)
            .map_or(0, |entry| entry.descriptor)
    };
    if private {
        let bytes = page.cell.bytes(held, &page.access)?;
        let (entries, []) = bytes.as_chunks_mut::<8>() else {
            unreachable!("native table geometry");
        };
        if entries.len() != ENTRIES {
            return Err(TableError::Layout);
        }
        for (index, entry) in entries.iter().enumerate() {
            if TablePage::read(entry) != descriptor(index) {
                return Err(TableError::Occupied);
            }
        }
    } else {
        for index in 0..ENTRIES {
            let word = page.cell.retained_word(held, &page.access, index)?;
            if u64::from_le(unsafe { word.as_ptr().read_volatile() }) != descriptor(index) {
                return Err(TableError::Occupied);
            }
        }
    }
    Ok(())
}

/// Check the complete existing private chain, from its own root to a
/// genuinely empty base-page table. Only the intended next descriptor
/// at each level may be nonzero. The exclusive preparation owner keeps
/// the chain unchanged until the top-level joint commit or rejection.
fn inspect_private(
    root: &'static TableStore,
    id: TableId,
    address: InternalAddress,
    mode: TurnMode,
) -> Result<Option<Expected>, TableError> {
    let mut position = Position {
        store: root,
        id,
        parent: None,
    };
    let mut head = None;
    for _ in 0..=root.level {
        let store = position.store;
        let index = coordinate(store.format, address, store.level).ok_or(TableError::Layout)?;
        let next = store.state.acquire(|state| {
            store.check(state, position.id)?;
            if store.level == 0 {
                if state.first_branch().is_some() {
                    return Err(TableError::Occupied);
                }
                return Ok(None);
            }
            let branch = state.branch(index).ok_or(TableError::Layout)?.1;
            if branch.detached
                || branch.child.level + 1 != store.level
                || branch.child.format != store.format
            {
                return Err(TableError::Scope);
            }
            Ok(Some(branch))
        })??;
        let expected = if let Some(branch) = next {
            let physical = branch.child.cell(branch.id)?.1.start();
            Some(Expected {
                branch,
                descriptor: store.format.branch(physical).ok_or(TableError::Layout)?,
            })
        } else {
            None
        };
        store.with_node(position.id, mode, |node, held| {
            let private = position.parent.is_none();
            let page = if private {
                node.private()?
            } else {
                position.page(node)?
            };
            check_page(page, held, expected, private)
        })?;
        if position.parent.is_none() {
            head = expected;
        }
        let Some(branch) = next else {
            return Ok(head);
        };
        position = Position {
            store: branch.child,
            id: branch.id,
            parent: Some((position.id, index)),
        };
    }
    Err(TableError::State)
}

impl LocalRoot<'_, '_> {
    pub(crate) fn has_pending_supply(&self) -> bool {
        self.supply.is_some()
    }

    /// Inspect the next missing table level in the actual owned path.
    /// None means a complete base-page path, not necessarily a vacant leaf.
    pub fn missing_table(
        &self,
        address: InternalAddress,
        mode: TurnMode,
    ) -> Result<Option<usize>, TableError> {
        match self.path(address, mode)? {
            Path::Missing { parent, .. } => Ok(Some(parent.store.level - 1)),
            Path::Leaf(_) => Ok(None),
        }
    }

    /// Accept the entire already linked private chain at the next missing
    /// position. All intermediate TablePages and return positions must
    /// already exist. The chain ends in one empty leaf table.
    /// Err leaves the original private subtree with its preparation owner;
    /// Ok transfers its top root and all descendants before native Make.
    ///
    /// # Safety
    /// The caller is the exclusive preparation owner of the private chain:
    /// exclude independent editors, retirement and republishing from the
    /// start of validation through its final commit. The original backing
    /// and working mappings are coherent table RAM and remain resident
    /// through root retirement. The admitted local context owns every
    /// native walk; this new path is not needed for execution continuity.
    pub unsafe fn supply_path(
        &mut self,
        address: InternalAddress,
        child: &'static TableStore,
        child_id: TableId,
        mode: TurnMode,
    ) -> Result<(), TableError> {
        let Path::Missing { parent, index } = self.path(address, mode)? else {
            return Err(TableError::Occupied);
        };
        let table = parent.store;
        if table.level != child.level + 1 || table.format != child.format {
            return Err(TableError::Layout);
        }
        let expected = inspect_private(child, child_id, address, mode)?;
        let branch = Branch {
            index,
            child,
            id: child_id,
            detached: false,
        };
        table.with_branch(parent.id, branch, mode, |held, parent_slot, child_slot| {
            let (state, mut locked) = parent_slot.together(held, &table.state);
            if state.branch(index).is_some() {
                return Err(TableError::Occupied);
            }
            let position = state.free_branch().ok_or(TableError::Exhausted)?;
            let page = parent.page(&mut state.node)?;
            let children = page.children.checked_add(1).ok_or(TableError::Exhausted)?;
            let word = page.cell.retained_word(&mut locked, &page.access, index)?;
            if unsafe { word.as_ptr().read_volatile() } != 0 {
                return Err(TableError::Occupied);
            }

            let (state, mut locked) = child_slot.together(held, &child.state);
            // Descendants are already retained by this exact private root.
            // Recheck that root and its original entry under the final Turn.
            check_children(state, expected)?;
            let page = state.node.private()?;
            check_page(page, &mut locked, expected, true)?;
            let physical = page.cell.physical().start();
            let encoded = table.format.branch(physical).ok_or(TableError::Layout)?;
            page.cell.freeze(&mut locked, &page.access)?;

            // All recoverable checks have succeeded. First publish custody
            // and the exact outstanding effect, then make the initialized
            // subtree hardware-reachable. No ordinary failure follows.
            page.exposure = Exposure::Child {
                parent: parent.id,
                index,
            };
            let (state, _) = parent_slot.together(held, &table.state);
            let page = parent
                .page(&mut state.node)
                .expect("original retained parent");
            page.children = children;
            state.set_branch(position, Some(branch));
            self.supply = Some(Pending {
                parent,
                branch,
                physical,
            });
            unsafe {
                self.control.order_table();
                word.as_ptr().write_volatile(encoded.to_le());
            }
            Ok(())
        })
    }

    /// Complete the original accepted non-leaf effect without holding a
    /// table Turn across hardware synchronization. Repeated completion
    /// after a failed acquisition retains the original pending record.
    ///
    /// # Safety
    /// The original local context and its retained table material remain
    /// resident under the same platform qualifications.
    pub unsafe fn advance_supply(&mut self, mode: TurnMode) -> Result<(), TableError> {
        let Some(pending) = self.supply.as_ref() else {
            return Ok(());
        };
        if self.installed().is_none() {
            return Err(TableError::State);
        }
        unsafe { self.control.synchronize_tree(self.next) }.map_err(TableError::Switch)?;
        let Pending {
            parent,
            branch,
            physical,
        } = *pending;
        parent
            .store
            .with_branch(parent.id, branch, mode, |held, parent_slot, child_slot| {
                let (state, mut locked) = parent_slot.together(held, &parent.store.state);
                let retained = state.branch(branch.index).ok_or(TableError::State)?.1;
                if retained.id != branch.id
                    || !core::ptr::eq(retained.child, branch.child)
                    || retained.detached
                {
                    return Err(TableError::Scope);
                }
                let page = parent.page(&mut state.node)?;
                let word = page
                    .cell
                    .retained_word(&mut locked, &page.access, branch.index)?;
                let observed = u64::from_le(unsafe { word.as_ptr().read_volatile() });
                if !parent.store.format.matches_branch(physical, observed) {
                    return Err(TableError::State);
                }

                let (state, _) = child_slot.together(held, &branch.child.state);
                let child = Position {
                    store: branch.child,
                    id: branch.id,
                    parent: Some((parent.id, branch.index)),
                };
                if child.page(&mut state.node)?.cell.physical().start() != physical {
                    return Err(TableError::Scope);
                }
                Ok(())
            })?;
        self.supply = None;
        Ok(())
    }
}
