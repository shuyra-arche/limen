//! Trusted local CPU facts and fixed storage.

use core::ptr;
#[cfg(limen_target)]
use core::{cell::Cell, ops::Range};

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

#[cfg(all(test, not(limen_target)))]
#[path = "../../../../../tests/metal/processor/hart/mod.rs"]
mod backend;
/// Verified native entry and the independent terminal stack exclusion.
#[cfg(limen_target)]
#[derive(Clone, Copy)]
pub(in crate::metal) struct Entry {
    native: backend::Entry,
    start: usize,
    end: usize,
}

#[cfg(limen_target)]
impl Entry {
    /// Check actual installed controls against the entry owner's expected
    /// representation. The caller still owns all vector/stack lifetimes.
    pub(in crate::metal) fn admit(
        expected: backend::Entry,
        terminal: Range<usize>,
    ) -> Option<Self> {
        if terminal.is_empty() || backend::entry() != Some(expected) {
            return None;
        }
        let entry = Self {
            native: expected,
            start: terminal.start,
            end: terminal.end,
        };
        entry.permits(expected).then_some(entry)
    }

    fn permits(self, observed: backend::Entry) -> bool {
        self.native == observed && !(self.start..self.end).contains(&backend::stack_pointer())
    }
}

#[cfg(limen_cooperative)]
#[derive(Clone, Copy)]
struct CooperativeBinding {
    scope: usize,
    slot: usize,
}

/// Fixed storage belonging to the verified physical CPU that took this entry.
pub(in crate::metal) struct HartLocal {
    #[cfg(limen_target)]
    identity: Cell<usize>,
    #[cfg(limen_target)]
    entry: Cell<Option<Entry>>,
    #[cfg(limen_cooperative)]
    cooperative: Cell<Option<CooperativeBinding>>,
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
            #[cfg(limen_target)]
            entry: Cell::new(None),
            #[cfg(limen_cooperative)]
            cooperative: Cell::new(None),
            protection: protection::LocalProtection::new(),
        }
    }

    /// The caller owns this freshly initialized CPU and permanent local storage.
    /// The actual entry owner keeps the verified vectors and terminal stack
    /// alive. No ordinary interrupt or continuation may enter during install.
    #[cfg(limen_target)]
    pub(in crate::metal) unsafe fn install(&'static self, identity: usize, entry: Entry) {
        assert!(backend::entry().is_some_and(|observed| entry.permits(observed)));
        assert!(self.entry.get().is_none());
        self.entry.set(Some(entry));
        self.identity.set(identity);
        unsafe { backend::install_local(ptr::from_ref(self).expose_provenance()) };
    }
}

fn installed() -> Option<&'static HartLocal> {
    // The trusted machine entry clears the local register before any ordinary
    // Rust call. Until another trusted entry takes over, it is either zero or
    // the exact installed permanent local address. This is not a mechanism
    // for interpreting an arbitrary inherited or user-controlled register.
    unsafe { ptr::with_exposed_provenance::<HartLocal>(backend::local_address()).as_ref() }
}

/// Only a trusted current kernel entry can acquire ordinary local protection.
#[cfg(limen_target)]
fn ordinary_local() -> Option<&'static HartLocal> {
    let observed = backend::entry()?;
    let local = installed()?;
    local.entry.get()?.permits(observed).then_some(local)
}

#[cfg(all(test, not(limen_target)))]
fn ordinary_local() -> Option<&'static HartLocal> {
    installed()
}

#[cfg(limen_target)]
pub(crate) fn current_hart() -> HartId {
    try_current_hart().expect("ordinary Hart entry with installed local storage required")
}

/// Fallible admission check for hardware consumers at their local boundary.
#[cfg(limen_target)]
pub(crate) fn try_current_hart() -> Option<HartId> {
    Some(HartId::new(ordinary_local()?.identity.get()))
}

#[cfg(limen_cooperative)]
pub(in crate::metal) fn cooperative_attached() -> bool {
    ordinary_local().is_some_and(|local| local.cooperative.get().is_some())
}

/// Same-Hart discovery of the permanently associated machine record.
#[cfg(limen_cooperative)]
pub(in crate::metal) fn cooperative_slot(scope: usize) -> Option<(HartId, usize)> {
    let local = ordinary_local()?;
    let binding = local.cooperative.get()?;
    (binding.scope == scope).then_some((HartId::new(local.identity.get()), binding.slot))
}

/// The caller has committed the current Hart's unique Room participation
/// under protection. The supplied machine record remains stable, is not
/// reused, and no ordinary entry may interleave this publication.
#[cfg(limen_cooperative)]
pub(in crate::metal) unsafe fn bind_cooperative(scope: usize, slot: usize) {
    let local = ordinary_local().expect("verified ordinary Hart");
    assert!(local.cooperative.get().is_none());
    local
        .cooperative
        .set(Some(CooperativeBinding { scope, slot }));
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
