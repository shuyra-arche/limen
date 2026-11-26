use core::{
    cell::UnsafeCell,
    mem::MaybeUninit,
    sync::atomic::Ordering::{Acquire, Release},
};

use crate::{
    address::length,
    entry::boot::Retirement,
    metal::identity::{Entry, GenerationTable},
    sync::{Acquisition, TurnLock, Work},
};

use super::{
    Allocator, Description, Error, RegionId,
    buddy::{Buddy, Geometry, Root},
    transfer::{AllocationRecord, AllocationScope, ReturnRecord, ReturnScope},
};

pub(super) struct Region {
    description: UnsafeCell<MaybeUninit<Description>>,
    pub(super) state: TurnLock<State>,
}

// The directory admits each slot once. No reader can reach its descriptor until
// the published prefix includes it (Release/Acquire). State uses its own Turn.
unsafe impl Sync for Region {}

pub(super) struct Backing {
    pub(super) roots: &'static mut [MaybeUninit<Root>],
    pub(super) nodes: &'static mut [u8],
}

enum GeometryState {
    Prepaid(Backing),
    Admitted(Buddy<'static>),
}

pub(super) struct State {
    geometry: Option<GeometryState>,
    pub(super) unassigned: u64,
    pub(super) allocations: GenerationTable<'static, AllocationScope, AllocationRecord>,
    pub(super) returns: GenerationTable<'static, ReturnScope, ReturnRecord>,
}

impl State {
    pub(super) fn buddy(&mut self) -> &mut Buddy<'static> {
        let Some(GeometryState::Admitted(buddy)) = self.geometry.as_mut() else {
            unreachable!("only published Regions have Buddy geometry");
        };
        buddy
    }

    pub(super) fn initialize(&mut self, geometry: Geometry, edits: usize, amount: u64) {
        let Some(GeometryState::Prepaid(storage)) = self.geometry.take() else {
            unreachable!("a directory slot is initialized once");
        };
        self.geometry = Some(GeometryState::Admitted(
            Buddy::initialize(geometry, storage.roots, storage.nodes, edits)
                .expect("geometry fits checked prepaid storage"),
        ));
        self.unassigned = amount;
    }
}

impl Region {
    pub(super) fn new(
        backing: Backing,
        allocations: &'static mut [Entry<AllocationRecord>],
        returns: &'static mut [Entry<ReturnRecord>],
        source: usize,
        index: usize,
    ) -> Self {
        Self {
            description: UnsafeCell::new(MaybeUninit::uninit()),
            state: TurnLock::new(State {
                geometry: Some(GeometryState::Prepaid(backing)),
                unassigned: 0,
                allocations: GenerationTable::new(AllocationScope(source, index), allocations)
                    .expect("checked fresh Allocation slots"),
                returns: GenerationTable::new(ReturnScope(source, index), returns)
                    .expect("checked fresh Return slots"),
            }),
        }
    }

    pub(super) fn initialize(
        &mut self,
        description: Description,
        geometry: Geometry,
        edits: usize,
    ) {
        self.state
            .get_mut()
            .initialize(geometry, edits, length(description.physical).get());
        self.description.get_mut().write(description);
    }

    pub(super) fn description(&self) -> Description {
        // Callers only reach this slot through the published prefix.
        unsafe { (*self.description.get()).assume_init() }
    }
}

/// Original delayed admission handoff. Keep this record with the retiring
/// owner. A retry after commit replays the exact Region identity.
#[must_use]
pub struct Grant {
    source: &'static Allocator,
    description: Description,
    admitted: Option<RegionId>,
}

impl Grant {
    /// # Safety
    ///
    /// The original owner must have exclusive custody of the ordinary RAM
    /// described here, including the right to give it to this source. It
    /// must never have been admitted to any source, and must exclude fixed
    /// metadata, other commitments, retained firmware and MMIO. The
    /// handoff must remain unique and the retirement evidence remain valid.
    pub unsafe fn new(
        source: &'static Allocator,
        description: Description,
        evidence: &Retirement,
    ) -> Result<Self, Error> {
        if !evidence.covers(description.physical) {
            return Err(Error::Range);
        }
        source.config.geometry(description)?;
        Ok(Self {
            source,
            description,
            admitted: None,
        })
    }

    pub fn admit(&mut self, mode: Acquisition) -> Result<RegionId, Error> {
        if let Some(id) = self.admitted {
            return Ok(id);
        }
        let source = self.source;
        let index = source.published.load(Acquire);
        let region = source.regions.get(index).ok_or(Error::Capacity)?;
        let description = self.description;
        let physical = description.physical;
        let geometry = source.config.geometry(description)?;
        let mut work = Work::<2>::new();
        let mut plan = work.plan();
        plan.insert(&source.directory).expect("directory");
        plan.insert(&region.state).expect("new Region");
        plan.execute(mode, |held| {
            if source.published.load(Acquire) != index {
                return Err(Error::Changed);
            }
            if source.root.overlaps(physical)
                || source
                    .regions()
                    .any(|(_, current)| current.physical.overlaps(physical))
            {
                return Err(Error::Overlap);
            }
            let total = source
                .regions()
                .try_fold(length(physical), |sum, (_, current)| {
                    sum.checked_add(length(current.physical))
                        .ok_or(Error::Capacity)
                })?;
            if total > source.config.maximum {
                return Err(Error::Capacity);
            }
            let state = held.access(&region.state).expect("new Region");
            state.initialize(geometry, source.config.edits, length(physical).get());
            // The directory owns the only write to this unpublished slot.
            unsafe { (*region.description.get()).write(description) };
            let id = RegionId(index);
            self.admitted = Some(id);
            source.published.store(index + 1, Release);
            Ok(id)
        })?
    }
}
