//! Legacy VT-d root, requester binding and register invalidation ownership.

use crate::metal::{
    memory::mmu::access::device::DeviceWindow, platform::description::acpi::RemappingUnit,
};
use limen_treaty::model::PhysicalAddress;

const VERSION: usize = 0;
const CAPABILITY: usize = 0x08;
const EXTENDED: usize = 0x10;
const COMMAND: usize = 0x18;
const STATUS: usize = 0x1c;
const ROOT: usize = 0x20;
const CONTEXT: usize = 0x28;
const FAULT_STATUS: usize = 0x34;
const FAULT_VALID: u64 = 1 << 63;
const FAULT_PENDING: u32 = 1 << 1;
const FAULT_OVERFLOW: u32 = 1;

const TRANSLATION: u32 = 1 << 31;
const ROOT_POINTER: u32 = 1 << 30;
const IN_PROGRESS: u64 = 1 << 63;
const GLOBAL_CONTEXT: u64 = 1 << 61;
const GLOBAL_IOTLB: u64 = 1 << 60;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Window,
    Unsupported,
    Occupied,
    Root,
    Pending,
    Unconfirmed,
    Capacity,
    Exhausted,
    Stale,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum State {
    Idle,
    LoadingRoot,
    InvalidatingContext,
    InvalidatingTranslations,
    RefreshingContexts,
    RefreshingTranslations,
    Enabling,
    Enabled,
    Unconfirmed,
}

/// Native legacy root, context and three-level second-level entries. The
/// caller owns the subordinate second-level translation tables.
#[derive(Clone, Copy)]
pub struct Format {
    address_mask: u64,
    domain_limit: u32,
}

impl Format {
    fn page(self, physical: PhysicalAddress) -> Option<u64> {
        let address = physical.get();
        (address != 0 && address & !self.address_mask == 0).then_some(address)
    }

    fn root_entry(self, context: PhysicalAddress) -> Option<[u64; 2]> {
        Some([self.page(context)? | 1, 0])
    }

    fn context_entry(self, root: PhysicalAddress, domain: u16) -> Option<[u64; 2]> {
        // DID zero is reserved on caching-mode implementations.
        if domain == 0 || u32::from(domain) >= self.domain_limit {
            return None;
        }
        Some([self.page(root)? | 1, (u64::from(domain) << 8) | 1])
    }

    pub fn table_entry(self, next: PhysicalAddress) -> Option<u64> {
        Some(self.page(next)? | 3)
    }

    pub fn read_only_leaf(self, page: PhysicalAddress) -> Option<u64> {
        Some(self.page(page)? | 1)
    }

    pub fn writable_leaf(self, page: PhysicalAddress) -> Option<u64> {
        Some(self.page(page)? | 3)
    }
}

/// One architectural root or context table. Both have 256 sixteen-byte entries.
#[repr(C, align(4096))]
pub struct Table([[u64; 2]; 256]);

impl Table {
    pub const EMPTY: Self = Self([[0; 2]; 256]);
}

/// Cache-line writeback and ordering for a native x86 table mapping.
pub struct Visibility {
    line: usize,
}

impl Visibility {
    #[cfg(all(limen_target, target_arch = "x86_64"))]
    pub fn native() -> Option<Self> {
        let features = core::arch::x86_64::__cpuid(1);
        let line = ((features.ebx >> 8) & 0xff) as usize * 8;
        (features.edx & (1 << 19) != 0 && line.is_power_of_two()).then_some(Self { line })
    }

    fn publish<T>(&self, value: &T) {
        let line = self.line;
        let pointer = core::ptr::from_ref(value).cast::<u8>();
        #[cfg(all(limen_target, target_arch = "x86_64"))]
        unsafe {
            use core::arch::x86_64::{_mm_clflush, _mm_mfence};
            for offset in (0..size_of_val(value)).step_by(line) {
                _mm_clflush(pointer.add(offset));
            }
            _mm_mfence();
        }
        #[cfg(not(all(limen_target, target_arch = "x86_64")))]
        {
            let _ = (line, pointer);
            unreachable!("VT-d table publication requires a native x86 target");
        }
    }
}

/// Resident sequence for this physical controller, preserved through resets
/// and re-admission so a stale binding cannot match a replacement.
pub struct Sequence(u64);

impl Sequence {
    pub const FRESH: Self = Self(0);

    fn reserve(&mut self, count: u64) -> Result<u64, Error> {
        self.0 = self.0.checked_add(count).ok_or(Error::Exhausted)?;
        Ok(self.0)
    }

    fn reserve_binding(
        &mut self,
        unit: PhysicalAddress,
        context: Context,
    ) -> Result<Binding, Error> {
        // An admitted binding can retire even if ordinary work consumes the
        // rest of this controller's sequence.
        let retirement_number = self.reserve(2)?;
        Ok(Binding {
            publication: CommandId {
                unit,
                number: retirement_number - 1,
            },
            retirement_number,
            context,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Context {
    pub requester: u16,
    pub domain: u16,
    pub root: PhysicalAddress,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Binding {
    publication: CommandId,
    retirement_number: u64,
    context: Context,
}

impl Binding {
    pub fn context(self) -> Context {
        self.context
    }

    fn retirement(self) -> CommandId {
        CommandId {
            unit: self.publication.unit,
            number: self.retirement_number,
        }
    }
}

/// The exact binding affected by an issued command sequence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Change {
    Bound(Binding),
    Invalidated(Binding),
    Unbound(Binding),
}

/// The original register-command sequence on this physical controller.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommandId {
    unit: PhysicalAddress,
    number: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Fault {
    slot: usize,
    address: u64,
    detail: u64,
}

impl Fault {
    pub fn address(self) -> u64 {
        self.address & !0xfff
    }

    pub fn requester(self) -> u16 {
        self.detail as u16
    }

    pub fn reason(self) -> u8 {
        (self.detail >> 32) as u8
    }

    pub fn write(self) -> bool {
        self.detail & (1 << 62) == 0
    }
}

enum Operation {
    Bind(usize),
    Invalidate(usize),
    Unbind(usize),
}

/// Root and context-page custody for one admitted PCI bus. Hardware storage
/// and its independently retired descendants outlive outstanding effects.
pub struct Storage<'a> {
    pub bus: u8,
    pub root: (&'a mut Table, PhysicalAddress),
    pub contexts: (&'a mut Table, PhysicalAddress),
    pub bindings: &'a mut [Option<Binding>],
    pub sequence: &'a mut Sequence,
}

#[must_use]
pub struct Controller<'a> {
    window: DeviceWindow<'a>,
    unit: RemappingUnit<'a>,
    storage: Storage<'a>,
    visibility: Visibility,
    pending: Option<(CommandId, Operation)>,
    fault_base: usize,
    fault_count: usize,
    iotlb: usize,
    drain: u64,
    format: Format,
    supports_39: bool,
    state: State,
    root: Option<PhysicalAddress>,
    unconfirmed: Option<State>,
}

impl<'a> Controller<'a> {
    /// # Safety
    /// The mapping must be the described legacy VT-d unit, little endian,
    /// strongly ordered and uncached. The caller holds exclusive command
    /// authority, with no firmware command in flight or other writer until
    /// the last effect settles. The root and context mappings are the
    /// corresponding aligned, pinned page addresses, with writable WB CPU
    /// mappings. Retain both pages and their descendants through hardware
    /// use, even after loss of this owner. The sequence remains bound to
    /// this physical unit across reset; only a never-used unit may start it
    /// fresh. Old effects must already be settled before re-admission.
    pub unsafe fn admit(
        unit: RemappingUnit<'a>,
        window: &'a mut DeviceWindow<'_>,
        storage: Storage<'a>,
        visibility: Visibility,
    ) -> Result<Self, Error> {
        if unit.registers() != window.physical() || !window.supports_u64(CONTEXT) {
            return Err(Error::Window);
        }
        let version = unsafe { window.read_u32(VERSION) }.map_err(|_| Error::Window)?;
        let cap = unsafe { window.read_u64(CAPABILITY) }.map_err(|_| Error::Window)?;
        let extended = unsafe { window.read_u64(EXTENDED) }.map_err(|_| Error::Window)?;
        // This path supports legacy register invalidation. Exclude scalable
        // mode hardware and required software write-buffer flushes here.
        if version >> 4 != 1
            || cap & 7 == 7
            || cap & (1 << 4) != 0
            || extended & (1 << 43) != 0
            || cap & (0x1f << 8) == 0
        {
            return Err(Error::Unsupported);
        }
        let offset = (((extended >> 8) & 0x3ff) * 16) as usize;
        let iotlb = offset + 8;
        if offset < 0x40 || !window.supports_u64(iotlb) {
            return Err(Error::Window);
        }
        let format = Format {
            domain_limit: 1u32 << (4 + 2 * (cap & 7)).min(16),
            address_mask: ((1u64 << unit.host_address_bits().min(52)) - 1) & !0xfff,
        };
        if format.page(storage.root.1).is_none()
            || format.page(storage.contexts.1).is_none()
            || storage.root.1 == storage.contexts.1
        {
            return Err(Error::Root);
        }
        if storage.bindings.is_empty() || storage.bindings.len() > storage.contexts.0.0.len() {
            return Err(Error::Capacity);
        }
        if storage.bindings.iter().any(Option::is_some)
            || storage.root.0.0.iter().any(|entry| *entry != [0; 2])
            || storage.contexts.0.0.iter().any(|entry| *entry != [0; 2])
        {
            return Err(Error::Occupied);
        }
        let fault_base = ((cap >> 24) & 0x3ff) as usize * 16;
        let fault_count = ((cap >> 40) & 0xff) as usize + 1;
        if !window.supports_u32(FAULT_STATUS)
            || fault_base < 0x40
            || !window.supports_u64(fault_base + 16 * fault_count - 8)
        {
            return Err(Error::Window);
        }
        let mut unit = Self {
            window: window.reborrow(),
            unit,
            storage,
            visibility,
            pending: None,
            fault_base,
            fault_count,
            iotlb,
            drain: ((cap >> 54) & 3) << 48,
            format,
            supports_39: cap & (1 << 9) != 0 && ((cap >> 16) & 0x3f) >= 38,
            state: State::Idle,
            root: None,
            unconfirmed: None,
        };
        if unit.status() != 0
            || unit.read(CONTEXT) & IN_PROGRESS != 0
            || unit.read(iotlb) & IN_PROGRESS != 0
        {
            return Err(Error::Occupied);
        }
        Ok(unit)
    }

    pub fn format39(&self) -> Option<Format> {
        self.supports_39.then_some(self.format)
    }

    pub fn state(&self) -> State {
        self.state
    }
    pub fn root(&self) -> Option<PhysicalAddress> {
        self.root
    }

    fn status(&mut self) -> u32 {
        unsafe { self.window.read_u32(STATUS) }.expect("validated VT-d status register")
    }
    fn read(&mut self, offset: usize) -> u64 {
        unsafe { self.window.read_u64(offset) }.expect("validated VT-d register")
    }
    fn write(&mut self, offset: usize, value: u64) {
        unsafe { self.window.write_u64(offset, value) }.expect("validated VT-d register");
    }
    fn unconfirmed(&mut self) -> Error {
        self.unconfirmed = Some(self.state);
        self.state = State::Unconfirmed;
        Error::Unconfirmed
    }
    fn command(&mut self, command: u32) {
        unsafe { self.window.write_u32(COMMAND, command) }
            .expect("validated VT-d command register");
    }

    /// Observe the first valid primary fault without clearing it.
    pub fn primary_fault(&mut self) -> Result<Option<Fault>, Error> {
        let status =
            unsafe { self.window.read_u32(FAULT_STATUS) }.expect("validated VT-d fault status");
        if status & FAULT_OVERFLOW != 0 {
            return Err(Error::Unconfirmed);
        }
        if status & FAULT_PENDING == 0 {
            return Ok(None);
        }
        let slot = ((status >> 8) & 0xff) as usize;
        if slot >= self.fault_count {
            return Err(Error::Unconfirmed);
        }
        let offset = self.fault_base + 16 * slot;
        let detail = self.read(offset + 8);
        let address = self.read(offset);
        if detail & FAULT_VALID == 0 || self.read(offset + 8) != detail {
            return Err(Error::Pending);
        }
        Ok(Some(Fault {
            slot,
            address,
            detail,
        }))
    }

    /// # Safety
    /// This is the original fault observed under this same controller.
    /// Quiesce its sources and exclude independent record acknowledgement
    /// until this exact record is cleared and the acknowledgement confirmed.
    pub unsafe fn acknowledge_fault(&mut self, expected: Fault) -> Result<(), Error> {
        if self.primary_fault()? != Some(expected) {
            return Err(Error::Stale);
        }
        let offset = self.fault_base + 16 * expected.slot + 8;
        self.write(offset, FAULT_VALID);
        (self.read(offset) & FAULT_VALID == 0)
            .then_some(())
            .ok_or(Error::Unconfirmed)
    }

    fn binding_slot(&self, requester: u16) -> Option<usize> {
        self.storage
            .bindings
            .iter()
            .position(|entry| entry.is_some_and(|binding| binding.context.requester == requester))
    }

    pub fn binding(&self, requester: u16) -> Option<Binding> {
        let slot = self.binding_slot(requester)?;
        match (&self.state, &self.pending) {
            (State::Enabled, None) => self.storage.bindings[slot],
            (
                State::RefreshingContexts | State::RefreshingTranslations,
                Some((_, Operation::Bind(changed) | Operation::Unbind(changed))),
            ) if *changed != slot => self.storage.bindings[slot],
            (State::RefreshingTranslations, Some((_, Operation::Invalidate(changed))))
                if *changed != slot =>
            {
                self.storage.bindings[slot]
            }
            _ => None,
        }
    }

    pub fn pending(&self) -> Option<(CommandId, Change)> {
        let (id, operation) = self.pending.as_ref()?;
        let change = match *operation {
            Operation::Bind(slot) => Change::Bound(self.storage.bindings[slot].unwrap()),
            Operation::Invalidate(slot) => {
                Change::Invalidated(self.storage.bindings[slot].unwrap())
            }
            Operation::Unbind(slot) => Change::Unbound(self.storage.bindings[slot].unwrap()),
        };
        Some((*id, change))
    }

    fn active_slot(&self, expected: Binding) -> Result<usize, Error> {
        if self.state != State::Enabled {
            return Err(Error::Occupied);
        }
        self.binding_slot(expected.context.requester)
            .filter(|&slot| self.storage.bindings[slot] == Some(expected))
            .ok_or(Error::Stale)
    }

    fn invalidate_contexts(&mut self) {
        self.state = State::RefreshingContexts;
        self.write(CONTEXT, IN_PROGRESS | GLOBAL_CONTEXT);
    }

    /// Publish a context for a previously blocked requester.
    ///
    /// # Safety
    /// The requester is actually routed to this unit, including topology and
    /// reserved-memory checks. Exclude aliases and ATS. Stop the requester
    /// and drain prior requests through native completion. The root and its
    /// descendants are initialized, pinned and visible to the walker.
    pub unsafe fn begin_bind(&mut self, context: Context) -> Result<CommandId, Error> {
        if self.state != State::Enabled {
            return Err(Error::Occupied);
        }
        let bus = (context.requester >> 8) as u8;
        let device = ((context.requester >> 3) & 31) as u8;
        let function = (context.requester & 7) as u8;
        if !self.supports_39
            || bus != self.storage.bus
            || !(self.unit.includes_all_pci()
                || self.unit.lists_root_endpoint(bus, device, function))
        {
            return Err(Error::Unsupported);
        }
        if context.root == self.storage.root.1 || context.root == self.storage.contexts.1 {
            return Err(Error::Root);
        }
        let entry = self
            .format
            .context_entry(context.root, context.domain)
            .ok_or(Error::Root)?;
        if self.storage.bindings.iter().flatten().any(|binding| {
            binding.context.requester == context.requester
                || binding.context.domain == context.domain
        }) {
            return Err(Error::Occupied);
        }
        let slot = self
            .storage
            .bindings
            .iter()
            .position(Option::is_none)
            .ok_or(Error::Capacity)?;
        let index = usize::from(context.requester as u8);
        if self.storage.contexts.0.0[index] != [0; 2] {
            return Err(Error::Occupied);
        }
        let binding = self
            .storage
            .sequence
            .reserve_binding(self.unit.registers().start(), context)?;
        let command = binding.publication;
        self.storage.bindings[slot] = Some(binding);
        self.pending = Some((command, Operation::Bind(slot)));
        let directory = &mut self.storage.contexts.0.0[index];
        directory[1] = entry[1];
        self.visibility.publish(directory);
        directory[0] = entry[0];
        self.visibility.publish(directory);
        self.invalidate_contexts();
        Ok(command)
    }

    /// Invalidate translations for the exact binding after a visible PTE edit.
    /// This restricted implementation uses a global hardware command.
    ///
    /// # Safety
    /// Serialize page-table edits, retain the old pages and quiesce affected
    /// requesters through native completion. Independent caches and device
    /// transactions require separate settlement.
    pub unsafe fn begin_invalidate(&mut self, expected: Binding) -> Result<CommandId, Error> {
        let slot = self.active_slot(expected)?;
        let command = CommandId {
            unit: self.unit.registers().start(),
            number: self.storage.sequence.reserve(1)?,
        };
        self.pending = Some((command, Operation::Invalidate(slot)));
        self.state = State::RefreshingTranslations;
        self.write(self.iotlb, IN_PROGRESS | GLOBAL_IOTLB | self.drain);
        Ok(command)
    }

    /// Block this requester and start retirement of its original binding.
    ///
    /// # Safety
    /// Exclude old device and fabric requests and keep the requester stopped
    /// through native completion. Retain the old tables and domain until the
    /// matching Unbound receipt. Separate endpoint obligations must settle.
    pub unsafe fn begin_unbind(&mut self, expected: Binding) -> Result<CommandId, Error> {
        let slot = self.active_slot(expected)?;
        let command = expected.retirement();
        let directory =
            &mut self.storage.contexts.0.0[usize::from(expected.context.requester as u8)];
        let original = self
            .format
            .context_entry(expected.context.root, expected.context.domain);
        self.pending = Some((command, Operation::Unbind(slot)));
        if Some(*directory) != original {
            return Err(self.unconfirmed());
        }
        *directory = [0; 2];
        self.visibility.publish(directory);
        self.invalidate_contexts();
        Ok(command)
    }

    pub fn observe(&mut self, expected: CommandId) -> Result<Change, Error> {
        let (command, completion) = self.pending().ok_or(Error::Stale)?;
        if command != expected {
            return Err(Error::Stale);
        }
        match self.state {
            State::RefreshingContexts => {
                self.context_complete()?;
                self.state = State::RefreshingTranslations;
                self.write(self.iotlb, IN_PROGRESS | GLOBAL_IOTLB | self.drain);
            }
            State::RefreshingTranslations => {}
            State::Unconfirmed => return Err(Error::Unconfirmed),
            _ => return Err(Error::Occupied),
        }
        self.translations_complete()?;
        if let Some((_, Operation::Unbind(slot))) = self.pending.take() {
            self.storage.bindings[slot] = None;
        }
        self.state = State::Enabled;
        Ok(completion)
    }

    pub fn unconfirmed_phase(&self) -> Option<State> {
        self.unconfirmed
    }

    fn context_complete(&mut self) -> Result<(), Error> {
        let actual = self.read(CONTEXT);
        if actual & IN_PROGRESS != 0 {
            return Err(Error::Pending);
        }
        if actual >> 59 & 3 != 1 {
            return Err(self.unconfirmed());
        }
        Ok(())
    }

    fn translations_complete(&mut self) -> Result<(), Error> {
        let actual = self.read(self.iotlb);
        if actual & IN_PROGRESS != 0 {
            return Err(Error::Pending);
        }
        if actual >> 57 & 3 != 1 {
            return Err(self.unconfirmed());
        }
        Ok(())
    }

    /// Install the retained, initially denying legacy root. Pending retries
    /// continue the original publication and preserve its pages.
    ///
    /// # Safety
    /// The entire aligned 4 KiB root must be addressable by this controller,
    /// initialized with valid scoped entries, visible to its walker and retained while
    /// installed or uncertain, including after this handle is dropped. All
    /// descendant tables, domain IDs and mapped pages must stay live for
    /// their hardware obligations. All covered requesters and the upstream
    /// interconnect must be quiesced through the transition.
    /// The operation does not cover ATS caches or revoke prior device access.
    pub unsafe fn install_legacy(&mut self) -> Result<(), Error> {
        let physical = self.storage.root.1;
        if self.state == State::Idle {
            let entry = &mut self.storage.root.0.0[usize::from(self.storage.bus)];
            let target = self
                .format
                .root_entry(self.storage.contexts.1)
                .ok_or(Error::Root)?;
            self.root = Some(physical);
            self.state = State::LoadingRoot;
            *entry = target;
            self.visibility.publish(&*self.storage.contexts.0);
            self.visibility.publish(&*self.storage.root.0);
            self.write(ROOT, physical.get());
            self.command(ROOT_POINTER);
        }
        for _ in 0..256 {
            let state = self.state;
            match state {
                State::LoadingRoot if self.status() & ROOT_POINTER != 0 => {
                    if self.read(ROOT) != physical.get() {
                        return Err(self.unconfirmed());
                    }
                    self.state = State::InvalidatingContext;
                    self.write(CONTEXT, IN_PROGRESS | GLOBAL_CONTEXT);
                }
                State::InvalidatingContext => match self.context_complete() {
                    Ok(()) => {
                        self.state = State::InvalidatingTranslations;
                        self.write(self.iotlb, IN_PROGRESS | GLOBAL_IOTLB | self.drain);
                    }
                    Err(Error::Pending) => core::hint::spin_loop(),
                    Err(error) => return Err(error),
                },
                State::InvalidatingTranslations => match self.translations_complete() {
                    Ok(()) => {
                        self.state = State::Enabling;
                        self.command(TRANSLATION);
                    }
                    Err(Error::Pending) => core::hint::spin_loop(),
                    Err(error) => return Err(error),
                },
                State::Enabling if self.status() & TRANSLATION != 0 => {
                    self.state = State::Enabled;
                    return Ok(());
                }
                State::Enabled => return Ok(()),
                State::RefreshingContexts | State::RefreshingTranslations => {
                    return Err(Error::Occupied);
                }
                State::Unconfirmed => return Err(Error::Unconfirmed),
                _ => core::hint::spin_loop(),
            }
        }
        Err(Error::Pending)
    }
}

#[cfg(test)]
#[path = "../../../../../../tests/metal/memory/mmu/x86_64/external/vtd.rs"]
mod tests;
