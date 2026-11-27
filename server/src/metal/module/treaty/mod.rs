//! Trusted module placement, exact interface uses and original retirement.

use core::{
    mem::{self, ManuallyDrop},
    sync::atomic::Ordering,
};
use limen_treaty::model::{InternalRange, PhysicalRange};

pub use super::image::placement::{Layout, PlacedSegment};
pub use super::image::{
    Description, DescriptionError, Error, Image, ImageAddress, Machine, ModuleImage,
    SegmentPermissions,
};
pub use super::installed::{Module, ModuleId, ModuleSlots, Retirement};
use super::installed::{OPEN, PHASE, RETIRED, RETIRING, SETTLING, SHUTTING, USE};
pub use super::interface::Interface;
mod retention;
pub use retention::ModuleUse;

/// Exact interface declaration to match. The consumer defines what the
/// interface and calling ABI mean.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Contract<'a> {
    compatible: &'a [u8],
    abi: u32,
    version: u32,
}

impl<'a> Contract<'a> {
    pub const fn new(compatible: &'a [u8], abi: u32, version: u32) -> Self {
        Self {
            compatible,
            abi,
            version,
        }
    }

    pub const fn compatible(self) -> &'a [u8] {
        self.compatible
    }

    pub const fn abi(self) -> u32 {
        self.abi
    }

    pub const fn version(self) -> u32 {
        self.version
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UseError {
    Interface,
    Capacity,
    Shut,
    Open,
    Busy,
    Retired,
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
