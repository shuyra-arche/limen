//! Legacy VT-d register invalidation and installation of a legacy translation root.
//! The caller owns requester quiescence and the retained root page.

use crate::metal::{
    memory::mmu::internal::device::DeviceWindow, platform::description::acpi::RemappingUnit,
};
use limen_treaty::model::PhysicalAddress;

const VERSION: usize = 0;
const CAPABILITY: usize = 0x08;
const EXTENDED: usize = 0x10;
const COMMAND: usize = 0x18;
const STATUS: usize = 0x1c;
const ROOT: usize = 0x20;
const CONTEXT: usize = 0x28;

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
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum State {
    Idle,
    LoadingRoot,
    InvalidatingContext,
    InvalidatingTranslations,
    RefreshingTranslations,
    Enabling,
    Enabled,
    Unconfirmed,
}

/// Native legacy root, context and three-level second-level entries. The
/// caller owns table storage, requester routing, ordering and visibility.
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

    pub fn root_entry(self, context: PhysicalAddress) -> Option<[u64; 2]> {
        Some([self.page(context)? | 1, 0])
    }

    pub fn context_entry(self, root: PhysicalAddress, domain: u16) -> Option<[u64; 2]> {
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

#[must_use]
pub struct Controller<'a> {
    window: &'a mut DeviceWindow,
    iotlb: usize,
    drain: u64,
    format: Format,
    supports_39: bool,
    state: State,
    root: Option<PhysicalAddress>,
}

impl<'a> Controller<'a> {
    /// # Safety
    /// The mapping must be the described legacy VT-d unit, little endian,
    /// strongly ordered and uncached. The caller holds exclusive command
    /// authority, with no firmware command in flight or other writer until
    /// the last effect settles. Independently retain the complete mapping.
    pub unsafe fn admit(
        unit: RemappingUnit<'_>,
        window: &'a mut DeviceWindow,
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
        let mut unit = Self {
            window,
            iotlb,
            drain: ((cap >> 54) & 3) << 48,
            format: Format {
                domain_limit: 1u32 << (4 + 2 * (cap & 7)).min(16),
                address_mask: ((1u64 << unit.host_address_bits().min(52)) - 1) & !0xfff,
            },
            supports_39: cap & (1 << 9) != 0 && ((cap >> 16) & 0x3f) >= 38,
            state: State::Idle,
            root: None,
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
        self.state = State::Unconfirmed;
        Error::Unconfirmed
    }
    fn command(&mut self, command: u32) {
        unsafe { self.window.write_u32(COMMAND, command) }
            .expect("validated VT-d command register");
    }

    /// Invalidate second-level translations after a visible PTE edit. Only
    /// one register command can be outstanding on this exclusive controller.
    ///
    /// # Safety
    /// Serialize page-table edits, quiesce all affected requesters and retain
    /// the old pages, translations and mappings through native completion.
    /// Independent ATS caches and outstanding device transactions need their
    /// own settlement. Context or root entry changes need separate commands.
    pub unsafe fn invalidate_translations(&mut self) -> Result<(), Error> {
        if self.state != State::Enabled {
            return Err(Error::Occupied);
        }
        self.state = State::RefreshingTranslations;
        self.write(self.iotlb, IN_PROGRESS | GLOBAL_IOTLB | self.drain);
        Ok(())
    }

    pub fn observe_invalidation(&mut self) -> Result<(), Error> {
        if self.state != State::RefreshingTranslations {
            return Err(Error::Occupied);
        }
        self.translations_complete()?;
        self.state = State::Enabled;
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

    /// Install a legacy root. Pending can be retried with the same
    /// physical page. An uncertain result retains all original obligations.
    ///
    /// # Safety
    /// The entire aligned 4 KiB root must be addressable by this controller,
    /// initialized with valid scoped entries, visible to its walker and retained while
    /// installed or uncertain, including after this handle is dropped. All
    /// descendant tables, domain IDs and mapped pages must stay live for
    /// their hardware obligations. All covered requesters and the upstream
    /// interconnect must be quiesced through the transition.
    /// The operation does not cover ATS caches or revoke prior device access.
    pub unsafe fn install_legacy(&mut self, physical: PhysicalAddress) -> Result<(), Error> {
        if self.format.page(physical).is_none() {
            return Err(Error::Root);
        }
        if let Some(original) = self.root {
            if original != physical {
                return Err(Error::Occupied);
            }
        } else {
            self.root = Some(physical);
            self.state = State::LoadingRoot;
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
                State::InvalidatingContext => {
                    let actual = self.read(CONTEXT);
                    if actual & IN_PROGRESS == 0 {
                        if actual >> 59 & 3 != 1 {
                            return Err(self.unconfirmed());
                        }
                        self.state = State::InvalidatingTranslations;
                        self.write(self.iotlb, IN_PROGRESS | GLOBAL_IOTLB | self.drain);
                    }
                }
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
                State::RefreshingTranslations => return Err(Error::Occupied),
                State::Unconfirmed => return Err(Error::Unconfirmed),
                _ => core::hint::spin_loop(),
            }
        }
        Err(Error::Pending)
    }
}
