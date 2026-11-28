//! Actual single-leaf translation within an installed, exclusively owned tree.

use limen_treaty::model::{InternalAddress, PhysicalAddress};

use super::{LocalRoot, TableError, TableId, TablePage, TableStore};
use crate::metal::{
    memory::mmu::{
        table::{ENTRIES, Exposure, Node, TableFormat},
        treaty::{LeafAccess, Memory},
    },
    sync::treaty::TurnMode,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TranslationPhase {
    Prepared,
    Installing,
    Active,
    Removing,
    Removed,
}

#[derive(Clone, Copy)]
struct Position {
    store: &'static TableStore,
    id: TableId,
    parent: Option<(TableId, usize)>,
}

impl Position {
    fn page(self, node: &mut Node) -> Result<&mut TablePage, TableError> {
        let Node::Admitted(page) = node else {
            return Err(TableError::State);
        };
        let exposed = match self.parent {
            None => matches!(page.exposure, Exposure::Local { .. }),
            Some((parent, index)) => page.exposure == Exposure::Child { parent, index },
        };
        if !page.initialized || !exposed {
            return Err(TableError::State);
        }
        Ok(page)
    }
}

/// The actual retained leaf reservation, including its unfinished hardware
/// effect. It cannot be copied or synthesized from an address. Dropping it
/// does not release its claim or permit root withdrawal.
#[must_use]
pub struct LocalTranslation {
    root: TableId,
    leaf: Position,
    index: usize,
    address: InternalAddress,
    physical: PhysicalAddress,
    access: LeafAccess,
    descriptor: u64,
    phase: TranslationPhase,
}

impl LocalTranslation {
    pub fn phase(&self) -> TranslationPhase {
        self.phase
    }

    pub(crate) fn physical(&self) -> PhysicalAddress {
        self.physical
    }

    pub(crate) fn access(&self) -> LeafAccess {
        self.access
    }
}

fn coordinate(format: TableFormat, address: InternalAddress, level: usize) -> Option<usize> {
    match format {
        TableFormat::Riscv64(format) => format.index(address, level),
        TableFormat::Aarch64(format) => format.index(address, level),
        TableFormat::X86_64(format) => format.index(address, level),
    }
}

fn claim(page: &TablePage, index: usize) -> bool {
    page.claims[index / 64] & (1 << (index % 64)) != 0
}

fn set_claim(page: &mut TablePage, index: usize, value: bool) {
    let bit = 1 << (index % 64);
    if value {
        page.claims[index / 64] |= bit;
    } else {
        page.claims[index / 64] &= !bit;
    }
}

fn matches_leaf(format: TableFormat, expected: u64, observed: u64) -> bool {
    match format {
        // Only native Accessed/Dirty changes are allowed in this IA-32e
        // profile. RISC-V sets A/D eagerly; Arm excludes hardware updates.
        TableFormat::X86_64(format) => format.matches_leaf(expected, observed),
        _ => observed == expected,
    }
}

impl LocalRoot<'_, '_> {
    /// Walk actual owned nodes, verifying the exact branch relationship and
    /// native descriptor. No borrowed firmware branch or superpage is
    /// treated as a privately editable leaf table.
    fn leaf_position(
        &self,
        address: InternalAddress,
        mode: TurnMode,
    ) -> Result<Position, TableError> {
        if self.installed().is_none() {
            return Err(TableError::State);
        }
        let mut position = Position {
            store: self.table,
            id: self.id,
            parent: None,
        };
        for _ in 0..self.table.format.levels() {
            let table = position.store;
            let index = coordinate(table.format, address, table.level).ok_or(TableError::Layout)?;
            if table.level == 0 {
                return Ok(position);
            }
            let branch = table.state.acquire(|state| {
                table.check(state, position.id)?;
                let branch = state.branch(index).ok_or(TableError::Scope)?.1;
                if branch.detached
                    || branch.child.level + 1 != table.level
                    || branch.child.format != table.format
                {
                    return Err(TableError::Scope);
                }
                Ok(branch)
            })??;
            let physical = branch.child.cell(branch.id)?.1.start();
            table.with_node(position.id, mode, |node, held| {
                let page = position.page(node)?;
                let word = page.cell.retained_word(held, &page.access, index)?;
                let observed = u64::from_le(unsafe { word.as_ptr().read_volatile() });
                if !table.format.matches_branch(physical, observed) {
                    return Err(TableError::Scope);
                }
                Ok(())
            })?;
            position = Position {
                store: branch.child,
                id: branch.id,
                parent: Some((position.id, index)),
            };
        }
        Err(TableError::State)
    }

    pub(super) fn vacant_leaf_range(
        &self,
        first: InternalAddress,
        last: InternalAddress,
        mode: TurnMode,
    ) -> Result<bool, TableError> {
        if first.get() >> 21 != last.get() >> 21 {
            return Ok(false);
        }
        let position = match self.leaf_position(first, mode) {
            Ok(position) => position,
            Err(TableError::Layout | TableError::Scope) => return Ok(false),
            Err(error) => return Err(error),
        };
        let start = coordinate(position.store.format, first, 0).ok_or(TableError::Layout)?;
        let end = coordinate(position.store.format, last, 0).ok_or(TableError::Layout)?;
        position.store.with_node(position.id, mode, |node, held| {
            let page = position.page(node)?;
            for index in start..=end {
                let word = page.cell.retained_word(held, &page.access, index)?;
                if claim(page, index) || unsafe { word.as_ptr().read_volatile() } != 0 {
                    return Ok(false);
                }
            }
            Ok(true)
        })
    }

    /// Prepare an actual vacant 4 KiB leaf in the owned tree. The descriptor
    /// is not written; the claim and original root block conflicting edits
    /// and retirement. The caller must retain this exact preparation.
    pub fn prepare_translation(
        &mut self,
        address: InternalAddress,
        physical: PhysicalAddress,
        access: LeafAccess,
        mode: TurnMode,
    ) -> Result<LocalTranslation, TableError> {
        if !address.get().is_multiple_of(4096) || access == LeafAccess::ReadExecute {
            return Err(TableError::Layout);
        }
        let leaf = self.leaf_position(address, mode)?;
        let index = coordinate(leaf.store.format, address, 0).ok_or(TableError::Layout)?;
        assert!(index < ENTRIES);
        let descriptor = leaf
            .store
            .format
            .leaf(physical, 0, access, Memory::Normal)
            .ok_or(TableError::Layout)?;
        let count = self
            .translations
            .checked_add(1)
            .ok_or(TableError::Exhausted)?;
        leaf.store.with_node(leaf.id, mode, |node, held| {
            let page = leaf.page(node)?;
            let word = page.cell.retained_word(held, &page.access, index)?;
            if claim(page, index) || unsafe { word.as_ptr().read_volatile() } != 0 {
                return Err(TableError::Occupied);
            }
            set_claim(page, index, true);
            Ok(())
        })?;
        self.translations = count;
        Ok(LocalTranslation {
            root: self.id,
            leaf,
            index,
            address,
            physical,
            access,
            descriptor,
            phase: TranslationPhase::Prepared,
        })
    }

    fn check_translation(&self, translation: &LocalTranslation) -> Result<(), TableError> {
        if translation.root != self.id || self.installed().is_none() {
            return Err(TableError::Scope);
        }
        Ok(())
    }

    /// Install the original descriptor, then complete its local native
    /// synchronization before reporting Active. No Turn is held during TLBI.
    ///
    /// # Safety
    /// The actual owner has accepted the exact backing physical use and
    /// compatible Normal RAM attributes and rights. Keep it live until
    /// removal completes. The prepared address is not required for the
    /// root's own execution, trap, stack or page-table access continuity.
    pub unsafe fn install_translation(
        &mut self,
        translation: &mut LocalTranslation,
        mode: TurnMode,
    ) -> Result<(), TableError> {
        self.check_translation(translation)?;
        match translation.phase {
            TranslationPhase::Prepared => {
                let leaf = translation.leaf;
                leaf.store.with_node(leaf.id, mode, |node, held| {
                    let page = leaf.page(node)?;
                    let word = page
                        .cell
                        .retained_word(held, &page.access, translation.index)?;
                    if !claim(page, translation.index)
                        || unsafe { word.as_ptr().read_volatile() } != 0
                    {
                        return Err(TableError::State);
                    }
                    translation.phase = TranslationPhase::Installing;
                    unsafe { word.as_ptr().write_volatile(translation.descriptor.to_le()) };
                    Ok(())
                })?;
            }
            TranslationPhase::Installing => {}
            TranslationPhase::Active => return Ok(()),
            _ => return Err(TableError::State),
        }
        unsafe {
            self.control
                .synchronize_leaf(self.next, translation.address)
        }
        .map_err(TableError::Switch)?;
        translation.phase = TranslationPhase::Active;
        Ok(())
    }

    /// Cancel a never-written preparation or withdraw the exact original
    /// installed leaf. A failed synchronization keeps the claim and all
    /// possible physical-access obligations in Removing.
    ///
    /// # Safety
    /// The original local owner excludes independent software/hardware
    /// users of this address, apart from the local cached translation
    /// effects that this operation retires. The backing physical use and
    /// all original table storage remain live until Removed.
    pub unsafe fn remove_translation(
        &mut self,
        translation: &mut LocalTranslation,
        mode: TurnMode,
    ) -> Result<(), TableError> {
        if translation.root != self.id {
            return Err(TableError::Scope);
        }
        if translation.phase == TranslationPhase::Removed {
            return Ok(());
        }
        self.check_translation(translation)?;
        let leaf = translation.leaf;
        let index = translation.index;
        let count = self.translations.checked_sub(1).ok_or(TableError::State)?;
        if matches!(
            translation.phase,
            TranslationPhase::Installing | TranslationPhase::Active
        ) {
            leaf.store.with_node(leaf.id, mode, |node, held| {
                let page = leaf.page(node)?;
                let word = page.cell.retained_word(held, &page.access, index)?;
                let observed = u64::from_le(unsafe { word.as_ptr().read_volatile() });
                if !claim(page, index)
                    || !matches_leaf(leaf.store.format, translation.descriptor, observed)
                {
                    return Err(TableError::State);
                }
                translation.phase = TranslationPhase::Removing;
                unsafe { word.as_ptr().write_volatile(0) };
                Ok(())
            })?;
        }
        if translation.phase == TranslationPhase::Removing {
            unsafe {
                self.control
                    .synchronize_leaf(self.next, translation.address)
            }
            .map_err(TableError::Switch)?;
        } else if translation.phase != TranslationPhase::Prepared {
            return Err(TableError::State);
        }
        leaf.store.with_node(leaf.id, mode, |node, held| {
            let page = leaf.page(node)?;
            let word = page.cell.retained_word(held, &page.access, index)?;
            if !claim(page, index) || unsafe { word.as_ptr().read_volatile() } != 0 {
                return Err(TableError::State);
            }
            set_claim(page, index, false);
            Ok(())
        })?;
        self.translations = count;
        translation.phase = TranslationPhase::Removed;
        Ok(())
    }
}
