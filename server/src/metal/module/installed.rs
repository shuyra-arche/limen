//! Installed modules retain their actual storage and dependencies.

use super::image::placement::Layout;
use super::treaty::ModuleUse;

use core::{
    fmt,
    mem::ManuallyDrop,
    ptr,
    sync::atomic::{AtomicUsize, Ordering},
};

// Closing admission and observing the final release cover the same
// population because the phase and use count share a modification order.
pub(super) const PHASE: usize = 0b111;
pub(super) const USE: usize = PHASE + 1;
const VACANT: usize = 0;
pub(super) const OPEN: usize = 1;
pub(super) const SHUTTING: usize = 2;
pub(super) const RETIRING: usize = 3;
pub(super) const SETTLING: usize = 4;
pub(super) const RETIRED: usize = 5;

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
pub struct ModuleId<'slot>(pub(super) &'slot AtomicUsize);

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
    pub(super) state: &'slot AtomicUsize,
    pub(super) layout: Layout<'source>,
    pub(super) material: ManuallyDrop<Material>,
    pub(super) dependencies: ManuallyDrop<[ModuleUse<'provider>; DEPENDENCIES]>,
}

/// Exclusive scope for withdrawal of the exact module. Dropping this before
/// completion retains its material and lets the same owner resume retirement.
#[must_use = "retirement needs real mapping settlement before material can be reclaimed"]
pub struct Retirement<'module> {
    pub(super) state: &'module AtomicUsize,
}

impl Drop for Retirement<'_> {
    fn drop(&mut self) {
        self.state.store(RETIRING, Ordering::Release);
    }
}
