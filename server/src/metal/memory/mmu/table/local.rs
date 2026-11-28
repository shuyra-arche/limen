//! Retain an owned tree through its original local native root installation.

use super::{Exposure, Node, TableError, TableId, TablePage, TableStore};
use limen_treaty::model::PhysicalAddress;
use crate::metal::{
    memory::mmu::treaty::{Context, LocalContext, SwitchError},
    sync::treaty::TurnMode,
};

#[derive(Clone, Copy, Eq, PartialEq)]
enum Stage {
    Prepared,
    Installing,
    Installed,
    Restoring,
    Restored,
    Finished,
}

/// An actual original firmware subtree retained through a local successor.
/// Its storage is never a second owned TablePage.
#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) struct BorrowedBranch {
    original: PhysicalAddress,
    index: usize,
    descriptor: u64,
}

/// The original owned root and its exclusive Processor control scope.
/// Dropping the handle leaves the table retained.
#[must_use]
pub struct LocalRoot<'control, 'scope> {
    table: &'static TableStore,
    id: TableId,
    control: &'control mut LocalContext<'scope>,
    previous: Context,
    next: Context,
    stage: Stage,
}

impl TableStore {
    /// Retain the original owned tree before publishing its native address.
    ///
    /// # Safety
    /// The caller keeps the original working mappings of every table page,
    /// the original context and all affected leaves valid. This processor
    /// exclusively owns the local installation and no other processor or
    /// device walks this tree. The actual mappings preserve code, stacks,
    /// handlers, tables and live data across both switches; executable and
    /// writable image ranges must retain their distinct permissions.
    pub unsafe fn prepare_local<'control, 'scope>(
        &'static self,
        id: TableId,
        control: &'control mut LocalContext<'scope>,
        mode: TurnMode,
    ) -> Result<LocalRoot<'control, 'scope>, TableError> {
        unsafe { self.retain_local(id, control, mode, None) }
    }

    /// Retain one actual supervisor subtree of the original IA-32e root
    /// together with this private successor. The original entry is not edited.
    ///
    /// # Safety
    /// In addition to prepare_local, original is the live retained identity
    /// mapping of the confirmed CR3 page. The selected subtree covers every
    /// inherited live access and its actual owner keeps all descendant
    /// storage, permissions, attributes and mappings stable and resident
    /// until the successor has been withdrawn and the borrow detached.
    /// No software edits those descriptors; native A/D updates are allowed.
    /// Other original roots and CPUs retain their own independent obligations.
    #[cfg(target_arch = "x86_64")]
    pub unsafe fn prepare_shared<'control, 'scope>(
        &'static self,
        id: TableId,
        control: &'control mut LocalContext<'scope>,
        original: core::ptr::NonNull<u64>,
        index: usize,
        mode: TurnMode,
    ) -> Result<LocalRoot<'control, 'scope>, TableError> {
        let previous = control.current().ok_or(TableError::State)?;
        let root = previous.root().ok_or(TableError::Scope)?;
        let descriptor = unsafe { control.original_branch(original, index) }
            .map_err(|_| TableError::Scope)?;
        let borrowed = BorrowedBranch {
            original: root,
            index,
            descriptor,
        };
        unsafe { self.retain_local(id, control, mode, Some(borrowed)) }
    }

    unsafe fn retain_local<'control, 'scope>(
        &'static self,
        id: TableId,
        control: &'control mut LocalContext<'scope>,
        mode: TurnMode,
        borrowed: Option<BorrowedBranch>,
    ) -> Result<LocalRoot<'control, 'scope>, TableError> {
        let previous = control.current().ok_or(TableError::State)?;
        if self.level + 1 != self.format.levels() {
            return Err(TableError::Layout);
        }
        let next = self.with_node(id, mode, |node, held| {
            let page = node.private()?;
            let next = unsafe { control.prepare_table(self.format, page.cell.physical().start()) }
                .map_err(|_| TableError::Scope)?;
            if let Some(branch) = borrowed {
                if previous.root() != Some(branch.original) {
                    return Err(TableError::Scope);
                }
                let entry = page.entry(held, branch.index)?;
                if TablePage::read(entry) != 0 {
                    return Err(TableError::Occupied);
                }
                TablePage::write(entry, branch.descriptor);
            }
            if let Err(error) = page.cell.freeze(held, &page.access) {
                if let Some(branch) = borrowed {
                    TablePage::write(
                        page.entry(held, branch.index).expect("unchanged private root"),
                        0,
                    );
                }
                return Err(error.into());
            }
            page.exposure = Exposure::Local { borrowed };
            Ok(next)
        })?;
        Ok(LocalRoot {
            table: self,
            id,
            control,
            previous,
            next,
            stage: Stage::Prepared,
        })
    }
}

impl LocalRoot<'_, '_> {
    /// Publish the original tree and settle its native installation.
    ///
    /// # Safety
    /// Continue the original tree and context obligations on this processor.
    pub unsafe fn install(&mut self) -> Result<(), SwitchError> {
        match self.stage {
            Stage::Prepared | Stage::Installing => {
                self.stage = Stage::Installing;
                unsafe { self.switch(self.previous, self.next) }?;
                self.stage = Stage::Installed;
                Ok(())
            }
            _ => Err(SwitchError::Current),
        }
    }

    /// Withdraw the original tree, with native maintenance and readback.
    ///
    /// # Safety
    /// Users of the candidate tree must have left and remain excluded,
    /// except the accesses needed for the verified switch itself.
    /// Continue both trees and their mappings through actual settlement.
    pub unsafe fn restore(&mut self) -> Result<(), SwitchError> {
        match self.stage {
            Stage::Installed | Stage::Restoring => {
                self.stage = Stage::Restoring;
                unsafe { self.switch(self.next, self.previous) }?;
                self.stage = Stage::Restored;
                Ok(())
            }
            _ => Err(SwitchError::Current),
        }
    }

    unsafe fn switch(&mut self, from: Context, to: Context) -> Result<(), SwitchError> {
        let installed = match self.control.pending() {
            Some(pair) if pair == (from, to) => unsafe { self.control.retry() }?,
            Some(_) => return Err(SwitchError::Unconfirmed),
            None => unsafe { self.control.install(from, to) }?,
        };
        if installed.previous() != from || installed.installed() != to {
            return Err(SwitchError::Unconfirmed);
        }
        Ok(())
    }

    /// Reopen the original access after withdrawal, or cancel before any write.
    pub fn finish(&mut self, mode: TurnMode) -> Result<(), TableError> {
        if !matches!(self.stage, Stage::Prepared | Stage::Restored) {
            return Err(TableError::State);
        }
        self.table.with_node(self.id, mode, |node, held| {
            let Node::Admitted(page) = node else {
                return Err(TableError::State);
            };
            let Exposure::Local { borrowed } = page.exposure else {
                return Err(TableError::State);
            };
            if borrowed.is_some_and(|branch| self.previous.root() != Some(branch.original)) {
                return Err(TableError::Scope);
            }
            // Either the native write never started, or the original control
            // has completed maintenance and confirmed withdrawal. Detach the
            // inherited descriptor before reopening ordinary root operations.
            unsafe { page.cell.thaw(held, &page.access) }?;
            if let Some(branch) = borrowed {
                TablePage::write(
                    page.entry(held, branch.index).expect("original retained branch"),
                    0,
                );
            }
            page.exposure = Exposure::Private;
            Ok(())
        })?;
        self.stage = Stage::Finished;
        Ok(())
    }
}
