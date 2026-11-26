//! Trusted local CPU facts and fixed storage.

#[cfg(limen_target)]
use core::cell::Cell;
use core::ptr;

mod node;
mod protection;
#[cfg(limen_target)]
pub(in crate::metal) mod stack;

#[cfg(all(limen_target, target_arch = "aarch64"))]
use super::aarch64::native as backend;
#[cfg(all(limen_target, target_arch = "riscv64"))]
use super::riscv::native as backend;
#[cfg(all(limen_target, target_arch = "x86_64"))]
use super::x86_64 as backend;

#[cfg(limen_target)]
pub(in crate::metal) use backend::stack_pointer;
pub(crate) use node::QueueNode;
pub(crate) use protection::{
    Error, TURN_CAPACITY, TurnParticipation, begin_leaf, begin_turn, can_switch,
};

#[cfg(limen_target)]
use crate::metal::platform::boot::ordinary_entry;

#[cfg(all(test, not(limen_target)))]
#[path = "../../../../../tests/metal/processor/hart/mod.rs"]
mod backend;
#[cfg(all(test, not(limen_target)))]
use backend::ordinary_entry;

/// Fixed storage belonging to the verified physical CPU that took this entry.
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

#[cfg(limen_target)]
pub(crate) fn current_hart() -> HartId {
    try_current_hart().expect("ordinary Hart entry with installed local storage required")
}

/// Fallible admission check for hardware consumers at their local boundary.
#[cfg(limen_target)]
pub(crate) fn try_current_hart() -> Option<HartId> {
    if !ordinary_entry() {
        return None;
    }
    Some(HartId::new(installed()?.identity.get()))
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
