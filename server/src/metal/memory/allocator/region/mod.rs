pub mod buddy;

use core::{cell::UnsafeCell, mem::MaybeUninit};

use super::{
    RegionDescription, RegionId,
    allocation::{AllocationRecord, AllocationScope},
    returns::{ReturnRecord, ReturnScope},
};
use crate::metal::{
    identity::treaty::{Entry, GenerationTable},
    sync::treaty::TurnLock,
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

    /// Set the initial descriptor before this slot becomes visible to readers.
    ///
    /// # Safety
    ///
    /// The caller must hold the directory's exclusive admission Turn and this
    /// slot must still lie beyond the published prefix.
    pub(super) unsafe fn publish_description(&self, description: RegionDescription) {
        unsafe { (*self.description.get()).write(description) };
    }

    pub(super) fn description(&self) -> RegionDescription {
        // Callers only reach this slot through the published prefix.
        unsafe { (*self.description.get()).assume_init() }
    }
}
