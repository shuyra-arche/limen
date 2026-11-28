//! Walk the original owned local tree, never a borrowed hardware path.

use limen_treaty::model::InternalAddress;

use super::{LocalRoot, TableError, TableId, TablePage, TableStore, translation::claim};
use crate::metal::{
    memory::mmu::table::{Exposure, Node, TableFormat},
    sync::treaty::TurnMode,
};

#[derive(Clone, Copy)]
pub(super) struct Position {
    pub(super) store: &'static TableStore,
    pub(super) id: TableId,
    pub(super) parent: Option<(TableId, usize)>,
}

impl Position {
    pub(super) fn page(self, node: &mut Node) -> Result<&mut TablePage, TableError> {
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

pub(super) enum Path {
    Missing { parent: Position, index: usize },
    Leaf(Position),
}

pub(super) fn coordinate(
    format: TableFormat,
    address: InternalAddress,
    level: usize,
) -> Option<usize> {
    match format {
        TableFormat::Riscv64(format) => format.index(address, level),
        TableFormat::Aarch64(format) => format.index(address, level),
        TableFormat::X86_64(format) => format.index(address, level),
    }
}

impl LocalRoot<'_, '_> {
    /// Inspect the exact owned branch and descriptor at every level. An
    /// invalid descriptor is a usable hole only when no original child
    /// occupies that entry. A nonzero descriptor without an owned branch
    /// may be a borrowed firmware path or a large leaf; reject it.
    pub(super) fn path(
        &self,
        address: InternalAddress,
        mode: TurnMode,
    ) -> Result<Path, TableError> {
        if self.installed().is_none() || self.has_pending_supply() {
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
                return Ok(Path::Leaf(position));
            }
            let branch = table.state.acquire(|state| {
                table.check(state, position.id)?;
                Ok::<_, TableError>(state.branch(index).map(|(_, branch)| branch))
            })??;
            let Some(branch) = branch else {
                return table.with_node(position.id, mode, |node, held| {
                    let page = position.page(node)?;
                    let word = page.cell.retained_word(held, &page.access, index)?;
                    if unsafe { word.as_ptr().read_volatile() } != 0 {
                        return Err(TableError::Scope);
                    }
                    Ok(Path::Missing {
                        parent: position,
                        index,
                    })
                });
            };
            if branch.detached
                || branch.child.level + 1 != table.level
                || branch.child.format != table.format
            {
                return Err(TableError::Scope);
            }
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

    /// Accept a range contained in one exact empty non-leaf entry, or a
    /// bounded interval of a real owned leaf table. The root owner has
    /// exclusive control of subsequent structural edits.
    pub(super) fn vacant_range(
        &self,
        first: InternalAddress,
        last: InternalAddress,
        mode: TurnMode,
    ) -> Result<bool, TableError> {
        match self.path(first, mode) {
            Ok(Path::Missing { parent, .. }) => {
                let shift = 12 + 9 * parent.store.level;
                Ok(first.get() >> shift == last.get() >> shift)
            }
            Ok(Path::Leaf(position)) => {
                if first.get() >> 21 != last.get() >> 21 {
                    return Ok(false);
                }
                let start =
                    coordinate(position.store.format, first, 0).ok_or(TableError::Layout)?;
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
            Err(TableError::Layout | TableError::Scope) => Ok(false),
            Err(error) => Err(error),
        }
    }
}
