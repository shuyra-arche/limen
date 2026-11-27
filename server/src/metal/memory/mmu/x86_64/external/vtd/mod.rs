//! Legacy VT-d root, requester binding and register invalidation ownership.

pub mod binding;
pub mod command;
pub mod directory;
pub mod fault;
pub mod translation;

use crate::metal::{
    memory::access::treaty::device::DeviceWindow, platform::description::acpi::RemappingUnit,
};
use command::{CommandId, Operation};
use directory::Storage;
use limen_treaty::model::PhysicalAddress;
use translation::{Format, Visibility};

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
}

#[cfg(test)]
#[path = "../../../../../../../tests/metal/memory/mmu/x86_64/external/vtd/mod.rs"]
mod tests;
