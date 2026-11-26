//! Exclusive direct-delivery domain. A single preallocated reception slot.

use super::{Description, IDC_BASE, IDC_BYTES, Mode, Route, SOURCE_LIMIT};
use crate::metal::processor::hart::local::current_hart;
use core::{arch::asm, marker::PhantomData, ptr};

const DOMAIN: usize = 0;
const DIRECT: u32 = 0x8000_0000;
const ENABLED: u32 = 1 << 8;
const PENDING: usize = 0x1c00;
const INPUT: usize = 0x1d00;
const ENABLE: usize = 0x1e00;
const SET_ENABLE: usize = 0x1edc;
const CLEAR_ENABLE: usize = 0x1fdc;
const TARGET: usize = 0x3000;
const DELIVERY: usize = 0;
const FORCE: usize = 4;
const THRESHOLD: usize = 8;
const CLAIM: usize = 28;
const EXTERNAL: usize = 1 << 9;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Failure {
    Context,
    Scope,
    Range,
    Busy,
    Unconfirmed,
    Exhausted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Record {
    round: u64,
    source: u32,
    priority: u8,
}

/// A borrow of this exact domain and reception round, not a reusable token.
/// Releasing the borrow never clears the owner's persistent claim record.
#[must_use]
pub struct Claim<'owner, 'description> {
    owner: &'owner mut Direct<'description>,
    record: Record,
}

impl Claim<'_, '_> {
    pub fn source(&self) -> u32 {
        self.record.source
    }

    pub fn priority(&self) -> u8 {
        self.record.priority
    }

    pub fn round(&self) -> u64 {
        self.record.round
    }

    /// # Safety
    /// Settle or durably hand off this exact software obligation. For a
    /// level interrupt, clear the device's original cause before settlement.
    pub unsafe fn settle(&mut self) -> Result<(), Failure> {
        self.owner.settle(self.record)
    }
}

#[derive(Clone, Copy)]
enum Reception {
    Empty,
    Receiving(u64),
    Claimed(Record),
    Uncertain(u64, u32),
}

/// One handed-over physical APLIC domain, with only its actual target IDC.
/// The caller retains the mapping, exclusive control and device obligations.
/// Dropping the software owner does not close or retire the hardware.
#[must_use]
pub struct Direct<'a> {
    description: Description<'a>,
    base: usize,
    modes: [Option<Mode>; SOURCE_LIMIT as usize + 1],
    reception: Reception,
    next: u64,
    opened: bool,
    uncertain: bool,
    local: PhantomData<*mut ()>,
}

impl<'a> Direct<'a> {
    /// # Safety
    /// The caller owns the entire supervisor direct-delivery domain and
    /// exclusive accesses to the selected Hart's external IRQ path. `base`
    /// maps the entire described physical window with valid device attributes
    /// and access rights for this lifetime. Old delivery, programming and
    /// device obligations are settled; no independent agent writes this
    /// domain. Preserve the original owner through shutdown and retirement.
    /// This first profile admits a single destination context and never
    /// accesses the machine-level parent domain.
    pub unsafe fn admit(description: Description<'a>, base: usize) -> Result<Self, Failure> {
        if description.contexts() != 1
            || base == 0
            || !base.is_multiple_of(4096)
            || description
                .registers()
                .length()
                .as_usize()
                .and_then(|length| base.checked_add(length))
                .is_none()
        {
            return Err(Failure::Scope);
        }
        let instance = Self {
            description,
            base,
            modes: [None; SOURCE_LIMIT as usize + 1],
            reception: Reception::Empty,
            next: 1,
            opened: false,
            uncertain: false,
            local: PhantomData,
        };
        instance.context()?;
        if instance.read(DOMAIN) != DIRECT
            || instance.read(instance.idc(DELIVERY)) != 0
            || instance.read(instance.idc(FORCE)) != 0
        {
            return Err(Failure::Busy);
        }
        for source in 1..=description.sources() {
            if instance.bit(ENABLE, source) || instance.bit(PENDING, source) {
                return Err(Failure::Busy);
            }
        }
        Ok(instance)
    }

    fn context(&self) -> Result<(), Failure> {
        let status: usize;
        unsafe { asm!("csrr {}, sstatus", out(reg) status, options(nostack, nomem)) };
        if current_hart().get() as u64 != self.description.hart() || status & 2 != 0 {
            return Err(Failure::Context);
        }
        Ok(())
    }

    fn available(&self) -> Result<(), Failure> {
        self.context()?;
        if self.uncertain || !matches!(self.reception, Reception::Empty) {
            return Err(Failure::Busy);
        }
        Ok(())
    }

    fn idc(&self, register: usize) -> usize {
        IDC_BASE + self.description.index() * IDC_BYTES + register
    }

    fn read(&self, offset: usize) -> u32 {
        // The constructor checked the whole window; private offsets are
        // bounded by its checked source/context geometry.
        unsafe {
            asm!("fence iorw, iorw", options(nostack));
            let value = ptr::without_provenance::<u32>(self.base + offset).read_volatile();
            asm!("fence iorw, iorw", options(nostack));
            value
        }
    }

    fn write(&mut self, offset: usize, value: u32) {
        unsafe {
            ptr::without_provenance_mut::<u32>(self.base + offset).write_volatile(value);
            asm!("fence iorw, iorw", options(nostack));
        }
    }

    fn bit(&self, register: usize, source: u32) -> bool {
        self.read(register + (source as usize / 32) * 4) & (1 << (source % 32)) != 0
    }

    fn unconfirmed(&mut self) -> Failure {
        // Retain the original owner and all possibly issued writes.
        self.uncertain = true;
        unsafe { asm!("csrc sie, {}", in(reg) EXTERNAL, options(nostack)) };
        Failure::Unconfirmed
    }

    /// # Safety
    /// The exact device source is stopped, all original effects are retired,
    /// and this is its sole legitimate programming owner. No pending input is
    /// discarded. Actual sourcecfg/target support is checked by readback.
    pub unsafe fn configure(&mut self, route: Route<'a>) -> Result<(), Failure> {
        self.available()?;
        if route.domain != self.description.source() || route.id > self.description.sources() {
            return Err(Failure::Scope);
        }
        let source = route.id;
        let index = source as usize;
        if self.modes[index].is_some() || self.bit(ENABLE, source) || self.bit(PENDING, source) {
            return Err(Failure::Busy);
        }
        let target = ((self.description.index() as u32) << 18) | 1;
        self.modes[index] = Some(route.mode);
        self.write(index * 4, route.mode as u32);
        self.write(TARGET + index * 4, target);
        if self.read(index * 4) != route.mode as u32 || self.read(TARGET + index * 4) != target {
            return Err(self.unconfirmed());
        }
        Ok(())
    }

    /// Enable an already prepared route, retaining all original pending bits.
    pub fn enable(&mut self, source: u32) -> Result<(), Failure> {
        self.available()?;
        self.mode(source)?;
        self.write(SET_ENABLE, source);
        if !self.bit(ENABLE, source) {
            return Err(self.unconfirmed());
        }
        Ok(())
    }

    fn mode(&self, source: u32) -> Result<Mode, Failure> {
        self.modes
            .get(source as usize)
            .copied()
            .flatten()
            .ok_or(Failure::Range)
    }

    /// # Safety
    /// Before global SIE is enabled, the caller installs a trusted reception
    /// path with enough capacity. This domain has one exclusive claim slot.
    pub unsafe fn open(&mut self) -> Result<(), Failure> {
        self.available()?;
        if self.opened {
            return Err(Failure::Busy);
        }
        self.write(self.idc(THRESHOLD), 0);
        self.write(self.idc(DELIVERY), 1);
        self.write(DOMAIN, ENABLED);
        if self.read(DOMAIN) != (DIRECT | ENABLED)
            || self.read(self.idc(DELIVERY)) != 1
            || self.read(self.idc(THRESHOLD)) != 0
        {
            return Err(self.unconfirmed());
        }
        self.opened = true;
        let enabled: usize;
        unsafe {
            asm!("csrs sie, {mask}", "csrr {enabled}, sie",
                mask = in(reg) EXTERNAL, enabled = out(reg) enabled, options(nostack));
        }
        if enabled & EXTERNAL == 0 {
            return Err(self.unconfirmed());
        }
        Ok(())
    }

    /// Reserve the only reception slot before the destructive claimi read.
    /// A nonempty claim remains recorded until explicitly settled.
    pub fn claim(&mut self) -> Result<Option<Claim<'_, 'a>>, Failure> {
        self.available()?;
        if !self.opened {
            return Err(Failure::Busy);
        }
        let round = self.next;
        self.next = round.checked_add(1).ok_or(Failure::Exhausted)?;
        self.reception = Reception::Receiving(round);
        let raw = self.read(self.idc(CLAIM));
        if raw == 0 {
            self.reception = Reception::Empty;
            return Ok(None);
        }
        let source = (raw >> 16) & 0x3ff;
        let priority = raw as u8;
        if raw & !0x03ff_00ff != 0 || priority == 0 || self.mode(source).is_err() {
            self.reception = Reception::Uncertain(round, raw);
            return Err(self.unconfirmed());
        }
        let claim = Record {
            round,
            source,
            priority,
        };
        self.reception = Reception::Claimed(claim);
        self.write(CLEAR_ENABLE, source);
        if self.bit(ENABLE, source) {
            return Err(self.unconfirmed());
        }
        Ok(Some(Claim {
            owner: self,
            record: claim,
        }))
    }

    /// Reborrow the original unclosed claim, without another destructive read.
    pub fn current(&mut self) -> Option<Claim<'_, 'a>> {
        match self.reception {
            Reception::Claimed(record) => Some(Claim {
                owner: self,
                record,
            }),
            _ => None,
        }
    }

    /// Retain raw evidence if a received identity could not be interpreted.
    pub fn uncertain_claim(&self) -> Option<(u64, u32)> {
        match self.reception {
            Reception::Uncertain(round, raw) => Some((round, raw)),
            Reception::Receiving(round) => Some((round, 0)),
            _ => None,
        }
    }

    // Only the original scoped Claim can invoke this operation.
    // Direct delivery has no Complete write; preserve later pending edges.
    fn settle(&mut self, claim: Record) -> Result<(), Failure> {
        self.context()?;
        if self.uncertain
            || !matches!(self.reception, Reception::Claimed(record) if record == claim)
        {
            return Err(Failure::Busy);
        }
        if self.bit(ENABLE, claim.source) {
            return Err(self.unconfirmed());
        }
        if matches!(self.mode(claim.source)?, Mode::LevelHigh | Mode::LevelLow)
            && (self.bit(INPUT, claim.source) || self.bit(PENDING, claim.source))
        {
            return Err(Failure::Busy);
        }
        self.reception = Reception::Empty;
        Ok(())
    }

    /// # Safety
    /// All configured devices have stopped producing old events and the
    /// caller has excluded all in-flight old programming and delivery.
    /// There is no generic device stop or DMA retirement implied here.
    pub unsafe fn shut(&mut self) -> Result<(), Failure> {
        self.available()?;
        unsafe { asm!("csrc sie, {}", in(reg) EXTERNAL, options(nostack)) };
        self.write(DOMAIN, 0);
        self.write(self.idc(DELIVERY), 0);
        self.opened = false;
        if self.read(DOMAIN) != DIRECT || self.read(self.idc(DELIVERY)) != 0 {
            return Err(self.unconfirmed());
        }
        for source in 1..=self.description.sources() {
            if self.mode(source).is_ok() {
                self.write(CLEAR_ENABLE, source);
                if self.bit(ENABLE, source) {
                    return Err(self.unconfirmed());
                }
                if self.bit(PENDING, source) {
                    return Err(Failure::Busy);
                }
            }
        }
        for source in 1..=self.description.sources() {
            if self.mode(source).is_ok() {
                self.write(source as usize * 4, 0);
                if self.read(source as usize * 4) != 0 {
                    return Err(self.unconfirmed());
                }
                self.modes[source as usize] = None;
            }
        }
        Ok(())
    }
}
