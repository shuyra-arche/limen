//! Permanent source identity and its fixed Region and Allowance storage.

use crate::metal::{identity::treaty::Slot, sync::treaty::TurnLock};
use core::{
    ptr,
    sync::atomic::{AtomicUsize, Ordering::Acquire},
};
use limen_treaty::model::PhysicalRange;

use super::{
    Config, Error, Region,
    allowance::{AllowanceSlot, Free},
    directory::Directory,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct SourceId(pub(super) usize);

/// A permanent position within the exact Allocator that admitted it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RegionId {
    pub(super) source: SourceId,
    slot: Slot,
}

impl RegionId {
    pub(super) fn new(source: SourceId, index: usize) -> Self {
        Self {
            source,
            slot: Slot::new(index).expect("checked Region capacity"),
        }
    }

    pub(super) fn index(self) -> usize {
        self.slot.index()
    }
}

/// A fixed source. Its root and canonical locks live for the runtime's lifetime.
pub struct Allocator {
    pub(super) config: Config,
    pub(super) root: PhysicalRange,
    pub(super) directory: TurnLock<Directory>,
    pub(super) published: AtomicUsize,
    pub(super) regions: &'static [Region],
    pub(super) allowances: &'static [TurnLock<AllowanceSlot>],
    pub(super) free: TurnLock<Free>,
}

impl Allocator {
    pub(super) fn source_id(&self) -> SourceId {
        // A published Allocator remains at the same root address for its
        // whole lifetime. The identity is used only for comparison.
        SourceId(ptr::from_ref(self).addr())
    }

    pub(super) fn region(&self, id: RegionId) -> Result<&Region, Error> {
        if id.source != self.source_id() || id.index() >= self.published.load(Acquire) {
            return Err(Error::Stale);
        }
        Ok(&self.regions[id.index()])
    }
}
