//! Supply an original empty table page to an installed owned path.

use limen_treaty::model::{InternalAddress, PhysicalAddress};

use super::{
    LocalRoot, TableError, TableId, TableStore,
    path::{Path, Position},
};
use crate::metal::{
    memory::mmu::table::{Exposure, TablePage, branch::Branch},
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

    /// Transfer one actual initialized empty private TablePage into the
    /// next missing position. On Err the original child remains private.
    /// On Ok the parent owns it and the native step is pending; the caller
    /// must advance the original supply, not submit the child a second time.
    ///
    /// # Safety
    /// The child's original backing and working mapping are compatible
    /// coherent table RAM and stay resident through the root's retirement.
    /// The exclusive admitted local context owns every native walk of this
    /// tree; the new path is not needed for current execution continuity.
    pub unsafe fn supply_table(
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
            let page = state.node.private()?;
            if page.children != 0 || page.claims.iter().any(|&bits| bits != 0) {
                return Err(TableError::Occupied);
            }
            let bytes = page.cell.bytes(&mut locked, &page.access)?;
            let (entries, []) = bytes.as_chunks_mut::<8>() else {
                unreachable!("native table geometry");
            };
            if entries.iter().any(|entry| TablePage::read(entry) != 0) {
                return Err(TableError::Occupied);
            }
            let physical = page.cell.physical().start();
            let encoded = table.format.branch(physical).ok_or(TableError::Layout)?;
            page.cell.freeze(&mut locked, &page.access)?;

            // All recoverable checks have succeeded. First publish custody
            // and the exact outstanding effect, then make the initialized
            // child hardware-reachable. No ordinary failure follows.
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
