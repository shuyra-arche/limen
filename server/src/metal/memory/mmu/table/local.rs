//! One owned tree in an originally inactive local TTBR1.

use super::{Exposure, Node, TableError, TableFormat, TableId, TableStore};
use crate::metal::{
    memory::mmu::{
        aarch64::internal::format::Root,
        treaty::{Context, LocalContext, SwitchError},
    },
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

/// The original owned root and its exclusive PE control scope.
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
    /// the whole lower translation context and all affected leaves valid.
    /// This PE exclusively owns the selected upper context and no other
    /// software or hardware walks the owned tree. Live code, stacks and
    /// handlers must stay in the retained lower context through both switches.
    pub unsafe fn prepare_local<'control, 'scope>(
        &'static self,
        id: TableId,
        control: &'control mut LocalContext<'scope>,
        mode: TurnMode,
    ) -> Result<LocalRoot<'control, 'scope>, TableError> {
        let previous = control.current().ok_or(TableError::State)?;
        let format = previous.profile();
        if format.root() != Root::Upper
            || previous.root().is_some()
            || self.format != TableFormat::Aarch64(format)
            || self.level + 1 != self.format.levels()
        {
            return Err(TableError::Layout);
        }
        let next = self.with_node(id, mode, |node, held| {
            let page = node.private()?;
            let next = unsafe { control.prepare_root(page.cell.physical().start()) }
                .map_err(|_| TableError::Scope)?;
            page.cell.freeze(held, &page.access)?;
            page.exposure = Exposure::Local;
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
    /// Continue the original tree and lower-context obligations on this PE.
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
    /// Upper users must have left and must remain excluded. Continue the
    /// original tree and lower-context obligations through settlement.
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
            if page.exposure != Exposure::Local {
                return Err(TableError::State);
            }
            // Either the native write never started, or the original control
            // has completed maintenance and confirmed withdrawal.
            unsafe { page.cell.thaw(held, &page.access) }?;
            page.exposure = Exposure::Private;
            Ok(())
        })?;
        self.stage = Stage::Finished;
        Ok(())
    }
}
