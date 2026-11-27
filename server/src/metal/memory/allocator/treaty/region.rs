//! The admitted range facts and original delayed admission authority.

use crate::metal::{
    identity::treaty::Slot,
    sync::treaty::{TurnMode, TurnWorkspace},
};
use core::sync::atomic::Ordering::{Acquire, Release};
use limen_treaty::model::PhysicalRange;

use super::{Allocator, Error, RegionId};

/// Provenance and raw attributes of the accepted RAM description.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegionOrigin {
    DeviceTree,
    Uefi { attributes: u64 },
    DeviceTreeAndUefi { attributes: u64 },
}

/// Checked physical geometry and its origin. This value carries no custody.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RegionDescription {
    pub physical: PhysicalRange,
    pub origin: RegionOrigin,
}

/// Original delayed admission handoff. Keep this record with the retiring
/// owner. A retry after commit replays the exact Region identity.
#[must_use]
pub struct Grant {
    source: &'static Allocator,
    description: RegionDescription,
    admitted: Option<RegionId>,
}

impl Grant {
    /// # Safety
    ///
    /// The original exclusive RAM owner must have verified that all old
    /// accesses covering this description are retired and that the RAM was
    /// never admitted, reused, or covered by a fixed claim. This is the
    /// unique handoff to the given source.
    pub(crate) unsafe fn from_retired(
        source: &'static Allocator,
        description: RegionDescription,
    ) -> Result<Self, Error> {
        source.config.geometry(description)?;
        Ok(Self {
            source,
            description,
            admitted: None,
        })
    }

    pub fn admit(&mut self, mode: TurnMode) -> Result<RegionId, Error> {
        if let Some(id) = self.admitted {
            return Ok(id);
        }
        let source = self.source;
        let index = source.published.load(Acquire);
        let region = source.regions.get(index).ok_or(Error::Capacity)?;
        let description = self.description;
        let physical = description.physical;
        let geometry = source.config.geometry(description)?;
        let mut work = TurnWorkspace::<2>::new();
        let mut plan = work.plan();
        plan.insert(&source.directory).expect("directory");
        plan.insert(&region.state).expect("new Region");
        plan.acquire(mode, |held| {
            if source.published.load(Acquire) != index {
                return Err(Error::Changed);
            }
            if source.root.overlaps(physical) {
                return Err(Error::Overlap);
            }
            let prepared = held.access(&source.directory).expect("directory").prepare(
                &source.regions[..index],
                physical,
                source.config.maximum,
            )?;
            let state = held.access(&region.state).expect("new Region");
            state.initialize(geometry, source.config.edits, physical.length().get());
            // This slot is unpublished under the exact directory Turn.
            unsafe { region.publish_description(description) };
            held.access(&source.directory)
                .expect("directory")
                .insert(Slot::new(index).expect("checked Region slot"), prepared);
            let id = RegionId::new(source.source_id(), index);
            self.admitted = Some(id);
            source.published.store(index + 1, Release);
            Ok(id)
        })?
    }
}
