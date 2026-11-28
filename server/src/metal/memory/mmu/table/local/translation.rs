//! Actual single-leaf translation within an installed, exclusively owned tree.

use limen_treaty::model::{InternalAddress, PhysicalAddress};

use super::{
    LocalRoot, TableError, TableId, TablePage,
    path::{Path, Position, coordinate},
};
use crate::metal::{
    memory::mmu::{
        table::{ENTRIES, TableFormat},
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

pub(super) fn claim(page: &TablePage, index: usize) -> bool {
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
        let Path::Leaf(leaf) = self.path(address, mode)? else {
            return Err(TableError::Scope);
        };
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
        if self.has_pending_supply() {
            return Err(TableError::State);
        }
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
