//! Trusted local CPU facts and fixed storage.

#[cfg(limen_target)]
use super::HartId;
#[cfg(limen_cooperative)]
use core::marker::PhantomData;
#[cfg(any(limen_target, test))]
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
use super::riscv64::native as backend;
#[cfg(all(limen_target, target_arch = "x86_64"))]
use super::x86_64::native as backend;

#[cfg(limen_target)]
pub(in crate::metal) use backend::stack_pointer;
pub(crate) use node::QueueNode;
#[cfg(any(limen_target, test))]
pub(crate) use protection::can_switch;
pub use protection::turn_capacity;
pub(crate) use protection::{Error, TurnParticipation, begin_leaf, begin_turn};

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

/// The permanent local state installed by the native major entry.
///
/// Startup can move this hardware relation to the cooperative constructor
/// once. Its use must still match the actual Hart and ordinary entry.
#[cfg(limen_cooperative)]
#[must_use]
pub struct Major {
    local: &'static HartLocal,
    hart_local: PhantomData<*mut ()>,
}

#[cfg(limen_cooperative)]
impl Major {
    pub(crate) fn is_current(&self) -> bool {
        ordinary_local().is_some_and(|local| ptr::eq(local, self.local))
    }
}

/// Resident storage supplied by one actual CPU's construction path.
#[cfg(any(limen_target, test))]
pub(in crate::metal) struct Storage<const N: usize> {
    local: HartLocal,
    nodes: [QueueNode; N],
}

#[cfg(any(limen_target, test))]
impl<const N: usize> Storage<N> {
    pub(in crate::metal) const fn new() -> Self {
        Self {
            local: HartLocal::new(),
            nodes: [const { QueueNode::new() }; N],
        }
    }

    /// The sole entering CPU binds its own resident nodes once, before
    /// installing any local register or admitting an ordinary operation.
    pub(in crate::metal) unsafe fn bind(&'static self) -> &'static HartLocal {
        self.local.protection.bind(&self.nodes);
        &self.local
    }
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
    #[cfg(any(limen_target, test))]
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

    /// # Safety
    /// Only the trusted native major entry may deliver this relation, once,
    /// from its own installed permanent local state and ordinary entry.
    #[cfg(limen_cooperative)]
    pub(in crate::metal) unsafe fn major(&'static self) -> Major {
        assert!(ordinary_local().is_some_and(|local| ptr::eq(local, self)));
        Major {
            local: self,
            hart_local: PhantomData,
        }
    }
}

#[cfg(any(limen_target, test))]
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

#[cfg(not(any(limen_target, test)))]
fn ordinary_local() -> Option<&'static HartLocal> {
    None
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
