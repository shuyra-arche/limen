//! Trusted local execution and fixed Executor storage.

#[cfg(limen_target)]
use crate::metal::processor::treaty::ProcessorId;
use core::cell::OnceCell;
#[cfg(limen_cooperative)]
use core::marker::PhantomData;
#[cfg(any(limen_target, test))]
use core::ptr;
#[cfg(limen_target)]
use core::{cell::Cell, ops::Range};

#[cfg(limen_target)]
use super::super::execution::runtime::RuntimeTrap;
use crate::metal::sync::local::LocalSync;
#[cfg(any(limen_target, test))]
use crate::metal::sync::local::{self as synchronization, Supply};

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
pub(in crate::metal) use protection::Protection;

#[cfg(all(test, not(limen_target)))]
#[path = "../../../../../tests/metal/processor/executor/mod.rs"]
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
/// once. Its use must still match the actual processor and ordinary entry.
#[cfg(limen_cooperative)]
#[must_use]
pub struct Major {
    local: &'static Executor,
    processor_local: PhantomData<*mut ()>,
}

#[cfg(limen_cooperative)]
impl Major {
    pub(crate) fn is_current(&self) -> bool {
        ordinary_local().is_some_and(|local| ptr::eq(local, self.local))
    }
}

/// Resident storage supplied by one actual CPU's construction path.
#[cfg(any(limen_target, test))]
pub struct LocalStorage<const N: usize> {
    local: Executor,
    synchronization: Supply<N>,
}

// Only the entering CPU binds or accesses the local records. Queue successors
// access individual published nodes through Sync's atomic links.
#[cfg(any(limen_target, test))]
unsafe impl<const N: usize> Sync for LocalStorage<N> {}

#[cfg(any(limen_target, test))]
impl<const N: usize> LocalStorage<N> {
    pub const fn new(synchronization: Supply<N>) -> Self {
        Self {
            local: Executor::new(),
            synchronization,
        }
    }

    pub(in crate::metal) const fn material(&'static self) -> Material {
        Material {
            local: &self.local,
            synchronization: self.synchronization.material(),
        }
    }
}

/// Fixed local material passed from the construction site to native startup.
#[cfg(any(limen_target, test))]
pub(in crate::metal) struct Material {
    local: &'static Executor,
    synchronization: synchronization::Material,
}

#[cfg(any(limen_target, test))]
impl Material {
    /// # Safety
    /// Only the entering CPU may establish the fixed synchronization
    /// association, before installing any local register or ordinary operation.
    pub(in crate::metal) unsafe fn bind(&self) -> &'static Executor {
        let synchronization = unsafe { self.synchronization.bind() };
        assert!(self.local.synchronization.set(synchronization).is_ok());
        self.local
    }
}

/// Fixed storage belonging to the verified physical CPU that took this entry.
pub(in crate::metal) struct Executor {
    #[cfg(limen_target)]
    identity: Cell<ProcessorId>,
    #[cfg(limen_target)]
    entry: Cell<Option<Entry>>,
    #[cfg(limen_cooperative)]
    cooperative: Cell<Option<CooperativeBinding>>,
    #[cfg(limen_target)]
    runtime: Cell<Option<&'static RuntimeTrap>>,
    protection: protection::LocalProtection,
    synchronization: OnceCell<&'static LocalSync>,
}

// The boot path installs this address only on the owning CPU. The only access
// from other CPUs is through the atomics in fixed queue nodes.
unsafe impl Sync for Executor {}

impl Executor {
    #[cfg(any(limen_target, test))]
    pub(in crate::metal) const fn new() -> Self {
        Self {
            #[cfg(limen_target)]
            identity: Cell::new(ProcessorId::new(0)),
            #[cfg(limen_target)]
            entry: Cell::new(None),
            #[cfg(limen_cooperative)]
            cooperative: Cell::new(None),
            #[cfg(limen_target)]
            runtime: Cell::new(None),
            protection: protection::LocalProtection::new(),
            synchronization: OnceCell::new(),
        }
    }

    #[cfg(limen_target)]
    pub(in crate::metal::processor) fn identity(&self) -> ProcessorId {
        self.identity.get()
    }

    /// The caller owns this freshly initialized CPU and permanent local storage.
    /// The actual entry owner keeps the verified vectors and terminal stack
    /// alive. No ordinary interrupt or continuation may enter during install.
    #[cfg(limen_target)]
    pub(in crate::metal) unsafe fn install(&'static self, identity: usize, entry: Entry) {
        assert!(backend::entry().is_some_and(|observed| entry.permits(observed)));
        assert!(self.entry.get().is_none());
        self.entry.set(Some(entry));
        self.identity.set(ProcessorId::new(identity));
        unsafe { backend::install_local(ptr::from_ref(self).expose_provenance()) };
    }

    #[cfg(limen_target)]
    pub(in crate::metal::processor) fn runtime(&self) -> Option<&'static RuntimeTrap> {
        self.runtime.get()
    }

    #[cfg(limen_target)]
    pub(in crate::metal::processor) fn publish_runtime(&self, runtime: &'static RuntimeTrap) {
        let entry = self.entry.get().expect("admitted local entry");
        self.runtime.set(Some(runtime));
        self.entry.set(Some(Entry {
            native: runtime.ordinary_entry(),
            ..entry
        }));
    }

    /// # Safety
    /// Only the trusted native major entry may deliver this relation, once,
    /// from its own installed permanent local state and ordinary entry.
    #[cfg(limen_cooperative)]
    pub(in crate::metal) unsafe fn major(&'static self) -> Major {
        assert!(ordinary_local().is_some_and(|local| ptr::eq(local, self)));
        Major {
            local: self,
            processor_local: PhantomData,
        }
    }
}

#[cfg(any(limen_target, test))]
fn installed() -> Option<&'static Executor> {
    // The trusted machine entry clears the local register before any ordinary
    // Rust call. Until another trusted entry takes over, it is either zero or
    // the exact installed permanent local address. This is not a mechanism
    // for interpreting an arbitrary inherited or user-controlled register.
    unsafe { ptr::with_exposed_provenance::<Executor>(backend::local_address()).as_ref() }
}

/// Only a trusted current kernel entry can acquire ordinary local protection.
#[cfg(limen_target)]
pub(in crate::metal::processor) fn ordinary_local() -> Option<&'static Executor> {
    let observed = backend::entry()?;
    let local = installed()?;
    if local
        .runtime()
        .is_some_and(|runtime| !runtime.ordinary_context())
    {
        return None;
    }
    local.entry.get()?.permits(observed).then_some(local)
}

/// Called from the recovering IRQ vector after saving the interrupted frame.
#[cfg(limen_target)]
pub(in crate::metal::processor) fn interrupt_local() -> Option<(ProcessorId, &'static RuntimeTrap)>
{
    let observed = backend::entry()?;
    let local = installed()?;
    let runtime = local.runtime()?;
    (runtime.interrupt_context(observed) && runtime.stack().contains(&backend::stack_pointer()))
        .then(|| (local.identity(), runtime))
}

#[cfg(all(test, not(limen_target)))]
pub(in crate::metal::processor) fn ordinary_local() -> Option<&'static Executor> {
    installed()
}

#[cfg(limen_cooperative)]
pub(in crate::metal) fn cooperative_attached() -> bool {
    ordinary_local().is_some_and(|local| local.cooperative.get().is_some())
}

/// Processor-local discovery of the permanently associated machine record.
#[cfg(limen_cooperative)]
pub(in crate::metal) fn cooperative_slot(scope: usize) -> Option<(ProcessorId, usize)> {
    let local = ordinary_local()?;
    let binding = local.cooperative.get()?;
    (binding.scope == scope).then_some((local.identity(), binding.slot))
}

/// The caller has committed the current processor's unique Room participation
/// under protection. The supplied machine record remains stable, is not
/// reused, and no ordinary entry may interleave this publication.
#[cfg(limen_cooperative)]
pub(in crate::metal) unsafe fn bind_cooperative(scope: usize, slot: usize) {
    let local = ordinary_local().expect("verified ordinary processor");
    assert!(local.cooperative.get().is_none());
    local
        .cooperative
        .set(Some(CooperativeBinding { scope, slot }));
}
