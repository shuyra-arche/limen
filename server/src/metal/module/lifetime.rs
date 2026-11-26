//! Publication and retained uses of modules in the boot owner's fixed slots.

use core::{
    fmt,
    mem::{self, ManuallyDrop},
    ops::Range,
    ptr,
    sync::atomic::{AtomicUsize, Ordering},
};

#[cfg(target_arch = "riscv64")]
use super::Sv39Root;
use super::{Abi, Contract, Layout, Machine};
use crate::address::PhysicalRange;

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
    Closed,
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
    /// giving the graph a publication order. Slot exhaustion or an invalid
    /// Sv39 dependency returns the caller's original layout, material and uses.
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
        if layout.image.description.abi() == Abi::Sv39Root
            && (sv39_geometry(&dependencies).is_none()
                || layout.entry(Contract::Sv39Root) == Some(0))
        {
            return Err((layout, material, dependencies));
        }
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

    pub fn virtual_range(&self) -> Range<u64> {
        self.layout.virtual_range()
    }

    /// Bind this encoder to its own retained geometry provider.
    #[cfg(target_arch = "riscv64")]
    pub fn bind_sv39(&self) -> Result<Sv39Root<'_>, UseError> {
        let geometry = sv39_geometry(self.dependencies.as_slice()).ok_or(UseError::Interface)?;
        let encoder = self.acquire(Contract::Sv39Root, 1)?;
        Ok(Sv39Root::new(encoder, geometry))
    }

    /// Acquire the exact published interface for a persistent consumer.
    /// Ordinary calls use this borrow and do not change the use count.
    pub fn acquire(&self, contract: Contract, version: u32) -> Result<ModuleUse<'_>, UseError> {
        let interface = self
            .layout
            .image
            .description
            .interface(contract)
            .filter(|interface| interface.version() == version)
            .ok_or(UseError::Interface)?;
        let entry = self.layout.place_entry(interface);

        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            if state & PHASE != OPEN {
                return Err(UseError::Closed);
            }
            let next = state.checked_add(USE).ok_or(UseError::Capacity)?;
            match self.state.compare_exchange_weak(
                state,
                next,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    return Ok(ModuleUse {
                        state: self.state,
                        entry,
                        contract,
                        version,
                        machine: self.layout.image.machine,
                    });
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

/// A nonduplicable use of one exact installed interface. Its actual consumer
/// must retain it through calls, continuations, and all cleanup. Transferring
/// the value transfers that retained use.
#[must_use = "keep this use while any consumer can reach the entry or its effects"]
pub struct ModuleUse<'module> {
    state: &'module AtomicUsize,
    entry: u64,
    contract: Contract,
    version: u32,
    machine: Machine,
}

impl<'module> ModuleUse<'module> {
    pub fn id(&self) -> ModuleId<'module> {
        ModuleId(self.state)
    }

    pub fn contract(&self) -> Contract {
        self.contract
    }

    pub fn version(&self) -> u32 {
        self.version
    }

    pub fn machine(&self) -> Machine {
        self.machine
    }

    /// Numeric entry for the consumer's previously validated domain ABI.
    /// Execution requires a live use and the corresponding execution scope.
    pub fn entry(&self) -> u64 {
        self.entry
    }
}

impl Drop for ModuleUse<'_> {
    fn drop(&mut self) {
        self.state.fetch_sub(USE, Ordering::Release);
    }
}

fn sv39_geometry<'borrow, 'provider>(
    dependencies: &'borrow [ModuleUse<'provider>],
) -> Option<&'borrow ModuleUse<'provider>> {
    let [geometry] = dependencies else {
        return None;
    };
    (geometry.contract() == Contract::MmuGeometry
        && geometry.version() == 1
        && geometry.machine() == Machine::Riscv64
        && geometry.entry() != 0)
        .then_some(geometry)
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
