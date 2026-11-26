//! Trusted local CPU facts and fixed storage.
#[cfg(limen_target)]
use core::cell::Cell;
use core::ptr;

#[path = "node.rs"]
mod node;
#[path = "protection.rs"]
mod protection;
#[cfg(limen_target)]
#[path = "stack.rs"]
pub(in crate::metal) mod stack;

#[cfg(all(limen_target, target_arch = "riscv64"))]
#[path = "riscv.rs"]
mod backend;
#[cfg(all(limen_target, target_arch = "x86_64"))]
#[path = "x86_64.rs"]
mod backend;
#[cfg(all(limen_target, target_arch = "aarch64"))]
#[path = "aarch64.rs"]
mod backend;

#[cfg(all(limen_target, any(target_arch = "aarch64", target_arch = "x86_64")))]
pub(in crate::metal) use backend::boot_identity;
#[cfg(all(limen_target, target_arch = "x86_64"))]
pub(in crate::metal) use backend::matches_madt;
#[cfg(limen_target)]
pub(in crate::metal) use backend::stack_pointer;
#[cfg(all(limen_target, target_arch = "aarch64"))]
pub use backend::{Aarch64Cpus, CpuCandidate};
pub(crate) use node::QueueNode;
#[cfg(all(limen_target, target_arch = "riscv64"))]
pub(crate) use protection::can_switch;
pub(crate) use protection::{Error, TURN_CAPACITY, TurnParticipation, begin_leaf, begin_turn};

/// Fixed storage belonging to the verified physical CPU that took this entry.
#[cfg(limen_target)]
use crate::metal::boot::ordinary_entry;

#[cfg(all(test, not(limen_target)))]
#[path = "../../../tests/metal/hart/mod.rs"]
mod backend;
#[cfg(all(test, not(limen_target)))]
use backend::ordinary_entry;

pub(in crate::metal) struct HartLocal {
    #[cfg(limen_target)]
    identity: Cell<usize>,
    protection: protection::LocalProtection,
}

// The boot path installs this address only on the owning CPU. The only access
// from other CPUs is through the atomics in fixed queue nodes.
unsafe impl Sync for HartLocal {}

impl HartLocal {
    pub(in crate::metal) const fn new() -> Self {
        Self {
            #[cfg(limen_target)]
            identity: Cell::new(0),
            protection: protection::LocalProtection::new(),
        }
    }

    /// The caller owns this freshly initialized CPU and permanent local storage.
    /// No ordinary interrupt or continuation may enter during installation.
    #[cfg(limen_target)]
    pub(in crate::metal) unsafe fn install(&'static self, identity: usize) {
        self.identity.set(identity);
        unsafe { backend::install_local(ptr::from_ref(self).expose_provenance()) };
    }
}

fn installed() -> Option<&'static HartLocal> {
    // Verified boot clears the local register before installing the vector,
    // then writes only the permanent local's address before ordinary entry.
    unsafe { ptr::with_exposed_provenance::<HartLocal>(backend::local_address()).as_ref() }
}

#[cfg(all(limen_target, target_arch = "x86_64"))]
pub(in crate::metal) unsafe fn clear_local() {
    unsafe { backend::install_local(0) };
}

#[cfg(limen_target)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct HartId(usize);

#[cfg(limen_target)]
impl HartId {
    pub(crate) const fn new(value: usize) -> Self {
        Self(value)
    }

    pub(crate) const fn get(self) -> usize {
        self.0
    }
}

#[cfg(limen_target)]
pub(crate) fn current_hart() -> HartId {
    assert!(ordinary_entry(), "ordinary Hart entry required");
    HartId::new(installed().expect("Hart installed at Boot").identity.get())
}
