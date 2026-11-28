//! Bounded base-page translation within an installed, exclusively owned tree.

use limen_treaty::model::{ByteLength, InternalAddress, InternalRange, PhysicalAddress, PhysicalRange};

use super::{
    LocalRoot, TableError, TableId, TablePage,
    path::{Path, Position, coordinate},
};
use crate::basis::cell::CellSlot;
use crate::metal::{
    memory::mmu::{
        table::{ENTRIES, TableFormat},
        treaty::{LeafAccess, Memory},
    },
    sync::treaty::{TurnMode, TurnWorkspace},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TranslationPhase {
    Prepared,
    Installing,
    Active,
    Removing,
    Removed,
}

const PAGE: u64 = 4096;
// Four contiguous base pages can require at most two leaf tables, hence
// four distinct TableStore/Cell Turns. This is a fixed first profile.
const LEAVES: usize = 4;

struct Leaf {
    position: Position,
    slot: &'static CellSlot,
    index: usize,
    descriptor: u64,
    phase: TranslationPhase,
}

/// One original bounded native effect. All leaf claims are reserved before
/// acceptance. Their separate phases retain exact partial native effects.
#[must_use]
pub struct LocalTranslation {
    root: TableId,
    range: InternalRange,
    physical: PhysicalRange,
    access: LeafAccess,
    leaves: [Option<Leaf>; LEAVES],
    phase: TranslationPhase,
}

impl LocalTranslation {
    pub fn phase(&self) -> TranslationPhase {
        self.phase
    }

    pub(crate) fn physical(&self) -> PhysicalRange {
        self.physical
    }

    pub(crate) fn range(&self) -> InternalRange {
        self.range
    }

    pub(crate) fn access(&self) -> LeafAccess {
        self.access
    }

    fn address(&self, index: usize) -> InternalAddress {
        self.range
            .start()
            .advance(ByteLength::new(index as u64 * PAGE))
            .expect("original checked translation range")
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
    /// Prepare the complete contiguous base-page range. The original
    /// caller supplies all actual intermediate table paths before this
    /// operation. No descriptor or claim is changed if any check fails.
    pub fn prepare_translation(
        &mut self,
        address: InternalAddress,
        physical: PhysicalAddress,
        length: ByteLength,
        access: LeafAccess,
        mode: TurnMode,
    ) -> Result<LocalTranslation, TableError> {
        if !address.get().is_multiple_of(PAGE)
            || !physical.get().is_multiple_of(PAGE)
            || length.get() == 0
            || !length.get().is_multiple_of(PAGE)
            || access == LeafAccess::ReadExecute
        {
            return Err(TableError::Layout);
        }
        let pages = length.get() / PAGE;
        if pages > LEAVES as u64 {
            return Err(TableError::Exhausted);
        }
        let range = address.range(length).ok_or(TableError::Layout)?;
        let physical = physical.range(length).ok_or(TableError::Layout)?;
        let count = self
            .translations
            .checked_add(1)
            .ok_or(TableError::Exhausted)?;
        let mut leaves = [const { None }; LEAVES];
        for (offset, target) in leaves.iter_mut().take(pages as usize).enumerate() {
            let offset = ByteLength::new(offset as u64 * PAGE);
            let address = range.start().advance(offset).expect("checked range");
            let physical = physical.start().advance(offset).expect("checked physical range");
            let Path::Leaf(position) = self.path(address, mode)? else {
                return Err(TableError::Scope);
            };
            let index = coordinate(position.store.format, address, 0).ok_or(TableError::Layout)?;
            assert!(index < ENTRIES);
            let descriptor = position
                .store
                .format
                .leaf(physical, 0, access, Memory::Normal)
                .ok_or(TableError::Layout)?;
            *target = Some(Leaf {
                position,
                slot: position.store.cell_slot(position.id)?,
                index,
                descriptor,
                phase: TranslationPhase::Prepared,
            });
        }

        let mut workspace = TurnWorkspace::<4>::new();
        let mut plan = workspace.plan();
        for leaf in leaves.iter().flatten() {
            plan.insert(&leaf.position.store.state)
                .expect("at most two original leaf tables");
            leaf.slot.plan(&mut plan);
        }
        plan.acquire(mode, |held| {
            // Recheck every actual claim under the same complete protection.
            for leaf in leaves.iter().flatten() {
                let position = leaf.position;
                let (state, mut cell) = leaf.slot.together(held, &position.store.state);
                position.store.check(state, position.id)?;
                let page = position.page(&mut state.node)?;
                let word = page.cell.retained_word(&mut cell, &page.access, leaf.index)?;
                if claim(page, leaf.index) || unsafe { word.as_ptr().read_volatile() } != 0 {
                    return Err(TableError::Occupied);
                }
            }
            // All checks have succeeded. No fallible claim or table allocation
            // follows. The same original protection covers the entire range.
            for leaf in leaves.iter().flatten() {
                let position = leaf.position;
                let (state, _) = leaf.slot.together(held, &position.store.state);
                let page = position.page(&mut state.node).expect("preflighted table");
                set_claim(page, leaf.index, true);
            }
            Ok::<(), TableError>(())
        })??;
        self.translations = count;
        Ok(LocalTranslation {
            root: self.id,
            range,
            physical,
            access,
            leaves,
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

    /// Advance one actual leaf. Active means every original leaf has
    /// completed its native synchronization, not just the current step.
    ///
    /// # Safety
    /// The actual owner has accepted the complete backing use, compatible
    /// Normal RAM attributes and rights. Keep the original material live
    /// until removal completes; no leaf is needed for execution continuity.
    pub unsafe fn install_translation(
        &mut self,
        translation: &mut LocalTranslation,
        mode: TurnMode,
    ) -> Result<(), TableError> {
        self.check_translation(translation)?;
        match translation.phase {
            TranslationPhase::Prepared | TranslationPhase::Installing => {}
            TranslationPhase::Active => return Ok(()),
            _ => return Err(TableError::State),
        }
        let offset = translation
            .leaves
            .iter()
            .position(|leaf| leaf.as_ref().is_some_and(|leaf| leaf.phase != TranslationPhase::Active))
            .expect("original unfinished leaf");
        let address = translation.address(offset);
        let leaf = translation.leaves[offset].as_mut().expect("original leaf");
        translation.phase = TranslationPhase::Installing;
        match leaf.phase {
            TranslationPhase::Prepared => {
                let position = leaf.position;
                position.store.with_node(position.id, mode, |node, held| {
                    let page = position.page(node)?;
                    let word = page.cell.retained_word(held, &page.access, leaf.index)?;
                    if !claim(page, leaf.index) || unsafe { word.as_ptr().read_volatile() } != 0 {
                        return Err(TableError::State);
                    }
                    leaf.phase = TranslationPhase::Installing;
                    unsafe { word.as_ptr().write_volatile(leaf.descriptor.to_le()) };
                    Ok(())
                })?;
            }
            TranslationPhase::Installing => {}
            _ => return Err(TableError::State),
        }
        unsafe { self.control.synchronize_leaf(self.next, address) }
            .map_err(TableError::Switch)?;
        leaf.phase = TranslationPhase::Active;
        if translation
            .leaves
            .iter()
            .flatten()
            .all(|leaf| leaf.phase == TranslationPhase::Active)
        {
            translation.phase = TranslationPhase::Active;
        }
        Ok(())
    }

    /// Advance one original leaf removal. A never-written leaf releases
    /// only its claim; a possibly visible leaf requires native retirement.
    /// The original range and accepted use survive until every leaf ends.
    ///
    /// # Safety
    /// The local owner excludes independent users of the whole range,
    /// apart from the original cached translations being retired. Retain
    /// the backing material and table storage until Removed.
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
        let count = self.translations.checked_sub(1).ok_or(TableError::State)?;
        translation.phase = TranslationPhase::Removing;
        let offset = translation
            .leaves
            .iter()
            .rposition(|leaf| leaf.as_ref().is_some_and(|leaf| leaf.phase != TranslationPhase::Removed))
            .expect("original unfinished retirement");
        let address = translation.address(offset);
        let leaf = translation.leaves[offset].as_mut().expect("original leaf");
        let position = leaf.position;
        if matches!(leaf.phase, TranslationPhase::Installing | TranslationPhase::Active) {
            position.store.with_node(position.id, mode, |node, held| {
                let page = position.page(node)?;
                let word = page.cell.retained_word(held, &page.access, leaf.index)?;
                let observed = u64::from_le(unsafe { word.as_ptr().read_volatile() });
                if !claim(page, leaf.index)
                    || !matches_leaf(position.store.format, leaf.descriptor, observed)
                {
                    return Err(TableError::State);
                }
                leaf.phase = TranslationPhase::Removing;
                unsafe { word.as_ptr().write_volatile(0) };
                Ok(())
            })?;
        }
        if leaf.phase == TranslationPhase::Removing {
            unsafe { self.control.synchronize_leaf(self.next, address) }
                .map_err(TableError::Switch)?;
        } else if leaf.phase != TranslationPhase::Prepared {
            return Err(TableError::State);
        }
        position.store.with_node(position.id, mode, |node, held| {
            let page = position.page(node)?;
            let word = page.cell.retained_word(held, &page.access, leaf.index)?;
            if !claim(page, leaf.index) || unsafe { word.as_ptr().read_volatile() } != 0 {
                return Err(TableError::State);
            }
            set_claim(page, leaf.index, false);
            Ok(())
        })?;
        leaf.phase = TranslationPhase::Removed;
        if translation
            .leaves
            .iter()
            .flatten()
            .all(|leaf| leaf.phase == TranslationPhase::Removed)
        {
            self.translations = count;
            translation.phase = TranslationPhase::Removed;
        }
        Ok(())
    }
}
