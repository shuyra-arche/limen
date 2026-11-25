//! Trusted local CPU facts and fixed storage.
use core::{cell::Cell, ptr};

mod node;
mod protection;
pub(in crate::metal) mod stack;

#[cfg(target_arch = "riscv64")]
#[path = "riscv.rs"]
mod backend;
#[cfg(target_arch = "x86_64")]
#[path = "x86_64.rs"]
mod backend;
#[cfg(target_arch = "aarch64")]
#[path = "aarch64.rs"]
mod backend;

#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
pub(in crate::metal) use backend::boot_identity;
pub(in crate::metal) use backend::stack_pointer;
pub(crate) use node::QueueNode;
#[cfg(target_arch = "riscv64")]
pub(crate) use protection::can_switch;
pub(crate) use protection::{
    Error, LeafParticipation, TURN_CAPACITY, TurnParticipation, begin_leaf, begin_turn,
};

/// Fixed storage belonging to the verified physical CPU that took this entry.
pub(in crate::metal) struct HartLocal {
    identity: Cell<usize>,
    protection: protection::LocalProtection,
}

// The boot path installs this address only on the owning CPU. The only access
// from other CPUs is through the atomics in fixed queue nodes.
unsafe impl Sync for HartLocal {}

impl HartLocal {
    pub(in crate::metal) const fn new() -> Self {
        Self {
            identity: Cell::new(0),
            protection: protection::LocalProtection::new(),
        }
    }

    /// The caller owns this freshly initialized CPU and permanent local storage.
    /// No ordinary interrupt or continuation may enter during installation.
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

fn ordinary_entry() -> bool {
    use crate::metal::boot;

    // Every terminal vector switches to the separate diagnostic stack before
    // running Rust or calling a platform diagnostic hook.
    boot::entry_installed() && !boot::trap_stack_range().contains(&backend::stack_pointer())
}

#[cfg(target_arch = "x86_64")]
pub(in crate::metal) unsafe fn clear_local() {
    unsafe { backend::install_local(0) };
}

pub(crate) fn current_hart() -> usize {
    assert!(ordinary_entry(), "ordinary Hart entry required");
    installed().expect("Hart installed at Boot").identity.get()
}
