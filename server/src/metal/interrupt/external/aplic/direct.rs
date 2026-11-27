//! Exclusive direct-delivery domain. A single preallocated reception slot.

use super::{
    Description, IDC_BASE, IDC_BYTES, Mode, Route, SOURCE_LIMIT,
    encoding::{DomainConfiguration, Interrupt, SourceConfiguration, Target},
};
use crate::metal::{
    interrupt::internal::supervisor::{Cause, Reception as LocalReception},
    memory::mmu::internal::device::DeviceWindow,
};
use core::{arch::asm, ptr};

const DOMAIN: usize = 0;
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Failure {
    Context,
    Scope,
    Range,
    Busy,
    Unconfirmed,
    Exhausted,
}

/// The actual domain lifecycle. A partial shut cannot be reopened.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum State {
    Prepared,
    Opening,
    Open,
    Shutting,
    Shut,
    Uncertain(Effect),
}

/// The exact operation whose original hardware effect was not confirmed.
/// Source and reception records retain the corresponding mode or claim.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Effect {
    Opening,
    Configure(u32),
    Enable(u32),
    Receive,
    Mask(u32),
    ShutDelivery,
    Retire(u32),
}

/// A source's locally owned programming state, indexed by its hardware ID.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Source {
    Vacant,
    Configuring(Mode),
    Masked(Mode),
    Enabling(Mode),
    Enabled(Mode),
    Masking(Mode),
    Retiring(Mode),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Record {
    round: u64,
    source: u32,
    priority: u8,
    raw: u32,
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
    Received(Record),
    Uncertain(u64, u32),
}

/// One handed-over physical APLIC domain, with only its actual target IDC.
/// The caller retains the mapping, exclusive control and device obligations.
/// Dropping the software owner does not close or retire the hardware.
#[must_use]
pub struct Direct<'a> {
    description: Description<'a>,
    mapping: &'a mut DeviceWindow,
    local: &'a mut LocalReception,
    sources: [Source; SOURCE_LIMIT as usize + 1],
    reception: Reception,
    next: u64,
    state: State,
}

impl<'a> Direct<'a> {
    /// # Safety
    /// The caller owns the entire supervisor direct-delivery domain and
    /// exclusive accesses to the selected Hart's external IRQ path. The
    /// borrowed mapping and local reception must remain valid through all
    /// hardware effects, including failure. Old delivery, programming and
    /// device obligations are settled; no independent agent writes this
    /// domain. Preserve the original owner through shutdown and retirement.
    /// No old source may remain configured, enabled or pending.
    /// This first profile admits a single destination context and never
    /// accesses the machine-level parent domain.
    pub unsafe fn admit(
        description: Description<'a>,
        mapping: &'a mut DeviceWindow,
        local: &'a mut LocalReception,
    ) -> Result<Self, Failure> {
        if description.contexts() != 1
            || mapping.physical() != description.registers()
            || !mapping.base().is_multiple_of(4096)
            || local.hart() != description.hart()
            || local.cause() != Cause::External
        {
            return Err(Failure::Scope);
        }
        let instance = Self {
            description,
            mapping,
            local,
            sources: [Source::Vacant; SOURCE_LIMIT as usize + 1],
            reception: Reception::Empty,
            next: 1,
            state: State::Prepared,
        };
        instance.context()?;
        if !DomainConfiguration::shut().confirms(instance.read(DOMAIN))
            || instance.read(instance.idc(DELIVERY)) != 0
            || instance.read(instance.idc(FORCE)) != 0
            || instance.local.enabled()
        {
            return Err(Failure::Busy);
        }
        for source in 1..=description.sources() {
            if !SourceConfiguration::Inactive.confirms(instance.read(source as usize * 4))
                || instance.bit(ENABLE, source)
                || instance.bit(PENDING, source)
            {
                return Err(Failure::Busy);
            }
        }
        Ok(instance)
    }

    fn context(&self) -> Result<(), Failure> {
        if !self.local.valid() {
            return Err(Failure::Context);
        }
        Ok(())
    }

    pub fn state(&self) -> State {
        self.state
    }

    pub fn source(&self, source: u32) -> Result<Source, Failure> {
        if !(1..=self.description.sources()).contains(&source) {
            return Err(Failure::Range);
        }
        Ok(self.sources[source as usize])
    }

    fn quiet(&self) -> Result<(), Failure> {
        self.context()?;
        if !matches!(self.reception, Reception::Empty) {
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
            let value = ptr::without_provenance::<u32>(
                self.mapping.word(offset).expect("checked APLIC offset"),
            )
            .read_volatile();
            asm!("fence iorw, iorw", options(nostack));
            value
        }
    }

    fn write(&mut self, offset: usize, value: u32) {
        unsafe {
            ptr::without_provenance_mut::<u32>(
                self.mapping.word(offset).expect("checked APLIC offset"),
            )
            .write_volatile(value);
            asm!("fence iorw, iorw", options(nostack));
        }
    }

    fn bit(&self, register: usize, source: u32) -> bool {
        self.read(register + (source as usize / 32) * 4) & (1 << (source % 32)) != 0
    }

    fn unconfirmed(&mut self, effect: Effect) -> Failure {
        // Preserve the original operation and any source or claim transition.
        // Attempt to stop local delivery; this is not proof of isolation.
        self.state = State::Uncertain(effect);
        self.local.set(false);
        Failure::Unconfirmed
    }

    /// # Safety
    /// The exact device source is stopped, all original effects are retired,
    /// and this is its sole legitimate programming owner. No pending input is
    /// discarded. Actual sourcecfg/target support is checked by readback.
    pub unsafe fn configure(&mut self, route: Route<'a>) -> Result<(), Failure> {
        self.quiet()?;
        if !matches!(self.state, State::Prepared | State::Open) {
            return Err(Failure::Busy);
        }
        if route.domain() != self.description.source() {
            return Err(Failure::Scope);
        }
        let source = route.id();
        let index = source as usize;
        if self.source(source)? != Source::Vacant
            || self.bit(ENABLE, source)
            || self.bit(PENDING, source)
        {
            return Err(Failure::Busy);
        }
        let mode = route.mode();
        let target = Target::new(self.description.index()).ok_or(Failure::Range)?;
        let configuration = SourceConfiguration::Active(mode);
        self.sources[index] = Source::Configuring(mode);
        self.write(index * 4, configuration.bits());
        self.write(TARGET + index * 4, target.bits());
        if !configuration.confirms(self.read(index * 4))
            || !target.confirms(self.read(TARGET + index * 4))
        {
            return Err(self.unconfirmed(Effect::Configure(source)));
        }
        self.sources[index] = Source::Masked(mode);
        Ok(())
    }

    /// Enable an already prepared route, retaining all original pending bits.
    pub fn enable(&mut self, source: u32) -> Result<(), Failure> {
        self.quiet()?;
        if self.state != State::Open {
            return Err(Failure::Busy);
        }
        self.next.checked_add(1).ok_or(Failure::Exhausted)?;
        let Source::Masked(mode) = self.source(source)? else {
            return Err(Failure::Busy);
        };
        self.sources[source as usize] = Source::Enabling(mode);
        self.write(SET_ENABLE, source);
        if !self.bit(ENABLE, source) {
            return Err(self.unconfirmed(Effect::Enable(source)));
        }
        self.sources[source as usize] = Source::Enabled(mode);
        Ok(())
    }

    // The source's original mode survives a failed or unconfirmed mask.
    fn mask(&mut self, source: u32, mode: Mode) -> Result<(), Failure> {
        self.sources[source as usize] = Source::Masking(mode);
        self.write(CLEAR_ENABLE, source);
        if self.bit(ENABLE, source) {
            return Err(self.unconfirmed(Effect::Mask(source)));
        }
        self.sources[source as usize] = Source::Masked(mode);
        Ok(())
    }

    /// # Safety
    /// Before global SIE is enabled, the caller installs a trusted reception
    /// path with enough capacity. This domain has one exclusive claim slot.
    pub unsafe fn open(&mut self) -> Result<(), Failure> {
        self.quiet()?;
        if self.state != State::Prepared {
            return Err(Failure::Busy);
        }
        self.state = State::Opening;
        self.write(self.idc(THRESHOLD), 0);
        self.write(self.idc(DELIVERY), 1);
        let configuration = DomainConfiguration::open();
        self.write(DOMAIN, configuration.write_bits());
        if !configuration.confirms(self.read(DOMAIN))
            || self.read(self.idc(DELIVERY)) != 1
            || self.read(self.idc(THRESHOLD)) != 0
        {
            return Err(self.unconfirmed(Effect::Opening));
        }
        self.local.set(true);
        if !self.local.enabled() {
            return Err(self.unconfirmed(Effect::Opening));
        }
        self.state = State::Open;
        Ok(())
    }

    /// Reserve the only reception slot before the destructive claimi read.
    /// A nonempty claim remains recorded until explicitly settled. On identity
    /// exhaustion the caller must not open another reception window.
    pub fn claim(&mut self) -> Result<Option<Claim<'_, 'a>>, Failure> {
        self.quiet()?;
        if self.state != State::Open {
            return Err(Failure::Busy);
        }
        let round = self.next;
        self.next = round.checked_add(1).ok_or(Failure::Exhausted)?;
        self.reception = Reception::Receiving(round);
        let raw = self.read(self.idc(CLAIM));
        let (source, priority) = match Interrupt::read(raw) {
            Some(Interrupt::Empty) => {
                self.reception = Reception::Empty;
                return Ok(None);
            }
            Some(Interrupt::Claimed { source, priority }) => (source, priority),
            None => {
                self.reception = Reception::Uncertain(round, raw);
                return Err(self.unconfirmed(Effect::Receive));
            }
        };
        let mode = match self.source(source) {
            Ok(Source::Enabled(mode)) if priority == Target::PRIORITY => mode,
            _ => {
                self.reception = Reception::Uncertain(round, raw);
                return Err(self.unconfirmed(Effect::Receive));
            }
        };
        let record = Record {
            round,
            source,
            priority,
            raw,
        };
        // Keep the original result before the next hardware effect (mask).
        self.reception = Reception::Received(record);
        self.mask(source, mode)?;
        Ok(Some(Claim {
            owner: self,
            record,
        }))
    }

    /// Reborrow the original confirmed claim without another destructive read.
    pub fn current(&mut self) -> Option<Claim<'_, 'a>> {
        match self.reception {
            Reception::Received(record)
                if self.state == State::Open
                    && matches!(self.source(record.source), Ok(Source::Masked(_))) =>
            {
                Some(Claim {
                    owner: self,
                    record,
                })
            }
            _ => None,
        }
    }

    /// Retain raw evidence if a received identity could not be interpreted.
    pub fn uncertain_claim(&self) -> Option<(u64, Option<u32>)> {
        match self.reception {
            Reception::Uncertain(round, raw) => Some((round, Some(raw))),
            Reception::Receiving(round) => Some((round, None)),
            Reception::Received(record) if matches!(self.state, State::Uncertain(_)) => {
                Some((record.round, Some(record.raw)))
            }
            _ => None,
        }
    }

    // Only the original scoped Claim can invoke this operation.
    // Direct delivery has no Complete write; preserve later pending edges.
    fn settle(&mut self, claim: Record) -> Result<(), Failure> {
        self.context()?;
        if self.state != State::Open
            || !matches!(self.reception, Reception::Received(record) if record == claim)
        {
            return Err(Failure::Busy);
        }
        let Source::Masked(mode) = self.source(claim.source)? else {
            return Err(Failure::Busy);
        };
        if self.bit(ENABLE, claim.source) {
            self.sources[claim.source as usize] = Source::Masking(mode);
            return Err(self.unconfirmed(Effect::Mask(claim.source)));
        }
        if matches!(mode, Mode::LevelHigh | Mode::LevelLow)
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
        self.quiet()?;
        match self.state {
            State::Shut => return Ok(()),
            State::Prepared | State::Open => {
                self.state = State::Shutting;
                self.local.set(false);
                let configuration = DomainConfiguration::shut();
                self.write(DOMAIN, configuration.write_bits());
                self.write(self.idc(DELIVERY), 0);
                if self.local.enabled()
                    || !configuration.confirms(self.read(DOMAIN))
                    || self.read(self.idc(DELIVERY)) != 0
                {
                    return Err(self.unconfirmed(Effect::ShutDelivery));
                }
            }
            State::Shutting => {}
            _ => return Err(Failure::Busy),
        }

        // First mask every enabled source. A pending source must not leave
        // a later source enabled when the operation returns Busy.
        for source in 1..=self.description.sources() {
            if let Source::Enabled(mode) = self.source(source)? {
                self.mask(source, mode)?;
            }
        }
        // No source is retired until the entire closing domain is drained.
        // Level inputs and edge pending are separate hardware evidence.
        for source in 1..=self.description.sources() {
            if let Source::Masked(mode) = self.source(source)? {
                if self.bit(ENABLE, source) {
                    self.sources[source as usize] = Source::Masking(mode);
                    return Err(self.unconfirmed(Effect::Mask(source)));
                }
                if self.bit(PENDING, source)
                    || (matches!(mode, Mode::LevelHigh | Mode::LevelLow) && self.bit(INPUT, source))
                {
                    return Err(Failure::Busy);
                }
            } else if self.source(source)? != Source::Vacant {
                return Err(Failure::Busy);
            }
        }
        for source in 1..=self.description.sources() {
            if let Source::Masked(mode) = self.source(source)? {
                let index = source as usize;
                self.sources[index] = Source::Retiring(mode);
                let configuration = SourceConfiguration::Inactive;
                self.write(index * 4, configuration.bits());
                if !configuration.confirms(self.read(index * 4)) {
                    return Err(self.unconfirmed(Effect::Retire(source)));
                }
                self.sources[index] = Source::Vacant;
            }
        }
        self.state = State::Shut;
        Ok(())
    }
}
