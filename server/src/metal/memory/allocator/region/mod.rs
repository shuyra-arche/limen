pub mod buddy;

use core::{
    cell::UnsafeCell,
    mem::MaybeUninit,
    sync::atomic::Ordering::{Acquire, Release},
};

use crate::metal::{
    identity::{Entry, GenerationTable, Slot},
    sync::{TurnLock, TurnMode, TurnWorkspace},
};
use limen_treaty::model::PhysicalRange;

use super::{
    Allocator, Error, RegionId,
    allocation::{AllocationRecord, AllocationScope},
    returns::{ReturnRecord, ReturnScope},
};

use buddy::{Buddy, Geometry, Root};

/// A local observation revision never wraps. Exhaustion prevents stitched observations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Revision {
    Current(u64),
    Exhausted,
}

impl Revision {
    pub(crate) fn next(self) -> Self {
        match self {
            Self::Current(version) if version != u64::MAX => Self::Current(version + 1),
            _ => Self::Exhausted,
        }
    }

    pub(crate) fn preserves(self, observed: Self) -> bool {
        matches!((self, observed), (Self::Current(now), Self::Current(before)) if now == before)
    }
}

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

pub(super) struct Region {
    description: UnsafeCell<MaybeUninit<RegionDescription>>,
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
    unassigned: u64,
    budget_revision: Revision,
    pub(super) allocations: GenerationTable<'static, AllocationScope, AllocationRecord>,
    pub(super) returns: GenerationTable<'static, ReturnScope, ReturnRecord>,
}

impl State {
    pub(super) fn unassigned(&self) -> u64 {
        self.unassigned
    }

    pub(super) fn budget(&self) -> (u64, Revision) {
        (self.unassigned, self.budget_revision)
    }

    pub(super) fn set_unassigned(&mut self, amount: u64) {
        if amount != self.unassigned {
            self.unassigned = amount;
            self.budget_revision = self.budget_revision.next();
        }
    }

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
        self.set_unassigned(amount);
    }
}

impl Region {
    pub(super) fn new(
        backing: Backing,
        allocations: &'static mut [Entry<AllocationRecord>],
        returns: &'static mut [Entry<ReturnRecord>],
        id: RegionId,
    ) -> Self {
        Self {
            description: UnsafeCell::new(MaybeUninit::uninit()),
            state: TurnLock::new(State {
                geometry: Some(GeometryState::Prepaid(backing)),
                unassigned: 0,
                budget_revision: Revision::Current(0),
                allocations: GenerationTable::new(AllocationScope(id), allocations)
                    .expect("checked fresh Allocation slots"),
                returns: GenerationTable::new(ReturnScope(id), returns)
                    .expect("checked fresh Return slots"),
            }),
        }
    }

    pub(super) fn initialize(
        &mut self,
        description: RegionDescription,
        geometry: Geometry,
        edits: usize,
    ) {
        self.state
            .get_mut()
            .initialize(geometry, edits, description.physical.length().get());
        self.description.get_mut().write(description);
    }

    pub(super) fn description(&self) -> RegionDescription {
        // Callers only reach this slot through the published prefix.
        unsafe { (*self.description.get()).assume_init() }
    }
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
            // The directory owns the only write to this unpublished slot.
            unsafe { (*region.description.get()).write(description) };
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
