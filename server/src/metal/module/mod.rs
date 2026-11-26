//! Publication and retained uses of modules in the boot owner's fixed slots.

mod image;
mod interface;

pub use image::{
    Description, DescriptionError, Error, Image, ImageAddress, Layout, Machine, ModuleImage,
    PlacedSegment, SegmentPermissions,
};
pub use interface::{Contract, Interface, ModuleUse};

use core::{
    fmt,
    mem::{self, ManuallyDrop},
    ptr,
    sync::atomic::{AtomicUsize, Ordering},
};

use limen_treaty::model::{InternalRange, PhysicalRange};

// Closing admission and observing the final release cover the same
// population because the phase and use count share a modification order.
const PHASE: usize = 0b111;
const USE: usize = PHASE + 1;
const VACANT: usize = 0;
const OPEN: usize = 1;
const SHUTTING: usize = 2;
const RETIRING: usize = 3;
const SETTLING: usize = 4;
const RETIRED: usize = 5;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UseError {
    Interface,
    Capacity,
    Shut,
    Open,
    Busy,
    Retired,
}

/// Fixed boot identity storage. Each position is issued once for the
/// life of this table. The owner supplies capacity for its boot sequence.
pub struct ModuleSlots<const SLOTS: usize> {
    states: [AtomicUsize; SLOTS],
}

impl<const SLOTS: usize> Default for ModuleSlots<SLOTS> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const SLOTS: usize> ModuleSlots<SLOTS> {
    pub const fn new() -> Self {
        Self {
            states: [const { AtomicUsize::new(VACANT) }; SLOTS],
        }
    }

    /// Publish the exact prepared image with its material and its immutable
    /// direct dependencies. Each dependency already has a live provider,
    /// giving the graph a publication order. Slot exhaustion returns the
    /// caller's original layout, material and uses.
    ///
    /// # Safety
    ///
    /// The owner must hold the supplied material and the exact original
    /// mapping. The image and dependencies must satisfy the caller's domain
    /// contracts. Effective permissions, absence of conflicting aliases and
    /// instruction visibility must cover the actual execution scope. The
    /// owner must keep these conditions true while entry or cleanup is
    /// possible, including across root changes. It must keep the returned
    /// module in durable owner storage until settlement and must not recover
    /// its backing or source through another route while it is retained.
    // Failure returns each owned input inline without allocating a recovery record.
    #[allow(clippy::result_large_err)]
    pub unsafe fn publish<'slot, 'source, 'provider, Material, const DEPENDENCIES: usize>(
        &'slot self,
        layout: Layout<'source>,
        material: Material,
        dependencies: [ModuleUse<'provider>; DEPENDENCIES],
    ) -> Result<
        Module<'slot, 'source, 'provider, Material, DEPENDENCIES>,
        (
            Layout<'source>,
            Material,
            [ModuleUse<'provider>; DEPENDENCIES],
        ),
    > {
        for state in &self.states {
            if state
                .compare_exchange(VACANT, OPEN, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                return Ok(Module {
                    state,
                    layout,
                    material: ManuallyDrop::new(material),
                    dependencies: ManuallyDrop::new(dependencies),
                });
            }
        }
        Err((layout, material, dependencies))
    }
}

/// Exact boot module identity. The borrowed slot cannot move while an identity
/// is observable, and it never identifies a later publication.
#[derive(Clone, Copy)]
pub struct ModuleId<'slot>(&'slot AtomicUsize);

impl PartialEq for ModuleId<'_> {
    fn eq(&self, other: &Self) -> bool {
        ptr::eq(self.0, other.0)
    }
}

impl Eq for ModuleId<'_> {}

impl fmt::Debug for ModuleId<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("ModuleId")
            .field(&ptr::from_ref(self.0))
            .finish()
    }
}

/// The actual installed module and its outstanding material and dependencies.
/// Dropping the owner without settlement retains the material and provider
/// obligations. Only reclaim may hand them back.
#[must_use = "an installed module owns material and must be kept until retirement"]
pub struct Module<'slot, 'source, 'provider, Material, const DEPENDENCIES: usize> {
    state: &'slot AtomicUsize,
    layout: Layout<'source>,
    material: ManuallyDrop<Material>,
    dependencies: ManuallyDrop<[ModuleUse<'provider>; DEPENDENCIES]>,
}

impl<'slot, Material, const DEPENDENCIES: usize> Module<'slot, '_, '_, Material, DEPENDENCIES> {
    pub fn id(&self) -> ModuleId<'slot> {
        ModuleId(self.state)
    }

    pub fn physical(&self) -> PhysicalRange {
        self.layout.physical()
    }

    pub fn internal(&self) -> InternalRange {
        self.layout.internal()
    }

    /// Borrow the exact direct uses retained by this installed image.
    pub fn dependencies(&self) -> &[ModuleUse<'_>] {
        self.dependencies.as_slice()
    }

    /// Acquire the exact published interface for a persistent consumer.
    /// Ordinary calls use this borrow and do not change the use count.
    pub fn acquire(&self, contract: Contract<'_>) -> Result<ModuleUse<'_>, UseError> {
        let (interface, entry) = self.layout.resolve(contract).ok_or(UseError::Interface)?;

        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            if state & PHASE != OPEN {
                return Err(UseError::Shut);
            }
            let next = state.checked_add(USE).ok_or(UseError::Capacity)?;
            match self.state.compare_exchange_weak(
                state,
                next,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    return Ok(ModuleUse::new(
                        self.state,
                        entry,
                        interface.contract(),
                        self.layout.machine(),
                    ));
                }
                Err(observed) => state = observed,
            }
        }
    }

    /// Close new admission. Existing uses keep their cleanup rights and
    /// continue to retain the exact image.
    pub fn shut(&self) {
        let _ = self
            .state
            .try_update(Ordering::AcqRel, Ordering::Relaxed, |state| {
                (state & PHASE == OPEN).then_some((state & !PHASE) | SHUTTING)
            });
    }

    /// Obtain exclusive retirement authority after every retained consumer
    /// has completed. This permits withdrawal. Actual completion remains the
    /// mapping owner's responsibility.
    pub fn retire(&self) -> Result<Retirement<'_>, UseError> {
        let mut state = self.state.load(Ordering::Acquire);
        loop {
            match state {
                SHUTTING | RETIRING => {}
                RETIRED => return Err(UseError::Retired),
                _ if state & PHASE == OPEN => return Err(UseError::Open),
                _ => return Err(UseError::Busy),
            }
            match self.state.compare_exchange_weak(
                state,
                SETTLING,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(Retirement { state: self.state }),
                Err(observed) => state = observed,
            }
        }
    }

    /// Return the original material once effects have settled and release the
    /// immutable provider uses. Early attempts return the same module owner.
    // An early return moves back the original custodian without allocating.
    #[allow(clippy::result_large_err)]
    pub fn reclaim(self) -> Result<Material, Self> {
        if self.state.load(Ordering::Acquire) != RETIRED {
            return Err(self);
        }
        drop(ManuallyDrop::into_inner(self.dependencies));
        Ok(ManuallyDrop::into_inner(self.material))
    }
}

/// Exclusive scope for withdrawal of the exact module. Dropping this before
/// completion retains its material and lets the same owner resume retirement.
#[must_use = "retirement needs real mapping settlement before material can be reclaimed"]
pub struct Retirement<'module> {
    state: &'module AtomicUsize,
}

impl Retirement<'_> {
    /// Settle the exact module after withdrawal.
    ///
    /// # Safety
    ///
    /// The original mapping owner must have excluded every future entry and
    /// settled cleanup, all in-flight effects, old translations and instruction
    /// fetch across the module's real execution and mapping scope. The
    /// retained material must be safe for its original owner to take back.
    pub unsafe fn complete(self) {
        self.state.store(RETIRED, Ordering::Release);
        mem::forget(self);
    }
}

impl Drop for Retirement<'_> {
    fn drop(&mut self) {
        self.state.store(RETIRING, Ordering::Release);
    }
}
