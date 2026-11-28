//! Stable address indexing for this source's append-only Region directory.

use core::cmp::Ordering;

use limen_treaty::model::{ByteLength, PhysicalRange};

use crate::metal::{
    identity::treaty::Slot,
    index::{Insertion as Position, Tree},
};

pub(super) use crate::metal::index::Link;

use super::{Error, Region};

/// The exact insertion decision remains private to the held directory.
pub(super) struct Insertion {
    position: Position,
    managed: ByteLength,
}

pub(super) struct Directory {
    tree: Tree,
    managed: ByteLength,
}

impl Directory {
    pub(super) fn new(links: &'static mut [Link]) -> Self {
        Self {
            tree: Tree::new(links),
            managed: ByteLength::new(0),
        }
    }

    pub(super) fn prepare(
        &self,
        regions: &[Region],
        range: PhysicalRange,
        maximum: ByteLength,
    ) -> Result<Insertion, Error> {
        let position = self
            .tree
            .prepare(|slot| {
                let physical = regions[slot.index()].description().physical;
                if range.end() <= physical.start() {
                    Ordering::Less
                } else if range.start() >= physical.end() {
                    Ordering::Greater
                } else {
                    Ordering::Equal
                }
            })
            .map_err(|_| Error::Overlap)?;
        let managed = self
            .managed
            .checked_add(range.length())
            .ok_or(Error::Capacity)?;
        if managed > maximum {
            return Err(Error::Capacity);
        }
        Ok(Insertion { position, managed })
    }

    pub(super) fn insert(&mut self, slot: Slot, plan: Insertion) {
        self.tree.insert(slot, plan.position);
        self.managed = plan.managed;
    }
}

#[cfg(test)]
#[path = "../../../../tests/metal/memory/allocator/directory.rs"]
pub(super) mod tests;
