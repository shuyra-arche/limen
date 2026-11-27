//! The one physical domain's shared configuration, sources and lifecycle.

use limen_treaty::model::{Alignment, ByteLength};

use super::{
    super::Barrier,
    Description, DomainDescription, Mode, Route, Source,
    direct::{self, Context, Receiver, encoding::Target},
    encoding::{DomainConfiguration, SourceConfiguration},
    registers::{Registers, Words},
    source::Sources,
};
use crate::metal::{
    interrupt::riscv64::internal::supervisor::Reception as LocalReception,
    memory::access::treaty::device::DeviceWindow,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Failure {
    Context,
    Scope,
    Range,
    Busy,
    Unconfirmed,
    Exhausted,
}

/// A partial shut cannot be reopened.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum State {
    Prepared,
    Opening,
    Open,
    Shutting,
    Shut,
    Uncertain(Effect),
}

/// The exact operation whose hardware effect was not confirmed.
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

/// Exclusive authority over the common registers and sources of one APLIC domain.
#[must_use]
pub struct Domain<'description, 'resources> {
    description: DomainDescription<'description>,
    registers: Registers<'resources>,
    sources: Sources<'resources>,
    state: State,
}

impl<'description, 'resources> Domain<'description, 'resources> {
    /// Take over a stopped direct-delivery domain and its one actual IDC.
    ///
    /// # Safety
    /// The caller exclusively owns the mapped supervisor domain and the original
    /// Hart's external reception. All prior source programming, delivery and
    /// device effects are settled. Keep the actual mapping and both owners alive
    /// until delivery and retirement are confirmed. Dropping either owner does
    /// not stop the hardware. The supplied storage covers the exact source
    /// count and starts vacant. No unsupported machine parent or extra IDC is
    /// accessed.
    pub unsafe fn admit(
        description: Description<'description>,
        mapping: &'resources mut DeviceWindow<'_>,
        storage: &'resources mut [Source],
        local: &'resources mut LocalReception,
        barrier: Barrier,
    ) -> Result<(Self, Context<'resources>), Failure> {
        let destination = description.destination();
        destination.verify_single()?;
        let description = description.domain();
        if mapping.physical() != description.registers()
            || !mapping.is_aligned(Alignment::new(4096).expect("APLIC page alignment"))
        {
            return Err(Failure::Scope);
        }
        let sources = Sources::admit(storage, description.sources())?;
        let (shared, tail) = mapping
            .reborrow()
            .split(ByteLength::new(direct::BASE as u64))
            .map_err(|_| Failure::Range)?;
        let length = ByteLength::new(direct::BYTES as u64);
        let idc = if tail.physical().length() == length {
            tail
        } else {
            tail.split(length).map_err(|_| Failure::Range)?.0
        };
        let mut context = Context::admit(
            description.registers(),
            destination,
            Words::new(idc, barrier),
            local,
        )?;
        let mut domain = Self {
            description,
            registers: Registers(Words::new(shared, barrier)),
            sources,
            state: State::Prepared,
        };
        if !domain.registers.confirms(DomainConfiguration::shut()) || !context.pristine() {
            return Err(Failure::Busy);
        }
        for source in 1..=description.sources() {
            if !SourceConfiguration::Inactive.confirms(domain.registers.source(source))
                || domain.registers.enabled(source)
                || domain.registers.pending(source)
            {
                return Err(Failure::Busy);
            }
        }
        Ok((domain, context))
    }

    pub fn state(&self) -> State {
        self.state
    }

    pub fn source(&self, source: u32) -> Result<Source, Failure> {
        self.sources.get(source)
    }

    /// Pair the exact original IDC with shared source control for one access.
    pub fn receiver<'access>(
        &'access mut self,
        context: &'access mut Context<'resources>,
    ) -> Result<Receiver<'access, 'description, 'resources>, Failure> {
        self.check(context)?;
        Ok(Receiver::new(self, context))
    }

    pub(super) fn check(&self, context: &Context<'_>) -> Result<(), Failure> {
        if context.domain() != self.description.registers() {
            return Err(Failure::Scope);
        }
        context.check()
    }

    fn quiet(&self, context: &Context<'_>) -> Result<(), Failure> {
        self.check(context)?;
        context.quiet()
    }

    pub(super) fn unconfirmed(&mut self, context: &mut Context<'_>, effect: Effect) -> Failure {
        self.state = State::Uncertain(effect);
        // An attempted mask does not prove that isolation is complete.
        context.set_enabled(false);
        Failure::Unconfirmed
    }

    /// Configure a device source to deliver through this original IDC.
    ///
    /// # Safety
    /// The exact device source has stopped, its previous effects are settled,
    /// and this is its sole programming owner. No pending input is discarded.
    pub unsafe fn configure(
        &mut self,
        context: &mut Context<'_>,
        route: Route<'description>,
    ) -> Result<(), Failure> {
        self.quiet(context)?;
        if !matches!(self.state, State::Prepared | State::Open) {
            return Err(Failure::Busy);
        }
        if route.domain() != self.description.source() {
            return Err(Failure::Scope);
        }
        let source = route.id();
        if self.source(source)? != Source::Vacant
            || self.registers.enabled(source)
            || self.registers.pending(source)
        {
            return Err(Failure::Busy);
        }
        let mode = route.mode();
        let target = Target::new(context.destination().index()).ok_or(Failure::Range)?;
        let configuration = SourceConfiguration::Active(mode);
        self.sources.set(source, Source::Configuring(mode));
        self.registers.set_source(source, configuration);
        self.registers.set_target(source, target.bits());
        if !configuration.confirms(self.registers.source(source))
            || !target.confirms(self.registers.target(source))
        {
            return Err(self.unconfirmed(context, Effect::Configure(source)));
        }
        self.sources.set(source, Source::Masked(mode));
        Ok(())
    }

    /// Enable the prepared source without dropping pending device effects.
    pub fn enable(&mut self, context: &mut Context<'_>, source: u32) -> Result<(), Failure> {
        self.quiet(context)?;
        if self.state != State::Open {
            return Err(Failure::Busy);
        }
        context.has_round()?;
        let Source::Masked(mode) = self.source(source)? else {
            return Err(Failure::Busy);
        };
        self.sources.set(source, Source::Enabling(mode));
        self.registers.set_enabled(source, true);
        if !self.registers.enabled(source) {
            return Err(self.unconfirmed(context, Effect::Enable(source)));
        }
        self.sources.set(source, Source::Enabled(mode));
        Ok(())
    }

    /// Mask future deliveries on the original source without discarding a
    /// pending input. The operator can resume the same binding to drain it.
    pub fn mask_source(&mut self, context: &mut Context<'_>, source: u32) -> Result<(), Failure> {
        self.quiet(context)?;
        match (self.state, self.source(source)?) {
            (State::Open, Source::Enabled(mode)) => self.mask(context, source, mode),
            (State::Open | State::Prepared, Source::Masked(_)) => Ok(()),
            (State::Uncertain(Effect::Mask(original)), Source::Masking(_))
                if original == source =>
            {
                self.confirm_mask(context, source)
            }
            _ => Err(Failure::Busy),
        }
    }

    /// Retire one source while its original IDC remains available to the
    /// other sources. A pending event retains the original configuration.
    ///
    /// # Safety
    /// The caller owns the bound device, has stopped new effects from its
    /// original use, and has excluded earlier effects still in transit to
    /// this controller. Preserve that exclusion and any outstanding software
    /// association until retirement succeeds. An already recorded claim or
    /// pending input still needs its own original delivery path.
    pub unsafe fn retire_source(
        &mut self,
        context: &mut Context<'_>,
        source: u32,
    ) -> Result<(), Failure> {
        self.quiet(context)?;
        if !matches!(self.state, State::Prepared | State::Open) {
            return Err(Failure::Busy);
        }
        match self.source(source)? {
            Source::Masked(mode) if self.source_quiet(source, mode) => {
                self.sources.set(source, Source::Retiring(mode));
                self.registers
                    .set_source(source, SourceConfiguration::Inactive);
            }
            Source::Retiring(_) => {}
            _ => return Err(Failure::Busy),
        }
        if !SourceConfiguration::Inactive.confirms(self.registers.source(source)) {
            return Err(Failure::Unconfirmed);
        }
        self.sources.set(source, Source::Vacant);
        Ok(())
    }

    fn source_quiet(&mut self, source: u32, mode: Mode) -> bool {
        !self.registers.enabled(source)
            && !self.registers.pending(source)
            && (!matches!(mode, Mode::LevelHigh | Mode::LevelLow) || !self.registers.input(source))
    }

    /// Keep the original mode even if masking is unconfirmed.
    pub(super) fn mask(
        &mut self,
        context: &mut Context<'_>,
        source: u32,
        mode: Mode,
    ) -> Result<(), Failure> {
        self.sources.set(source, Source::Masking(mode));
        self.registers.set_enabled(source, false);
        if self.registers.enabled(source) {
            return Err(self.unconfirmed(context, Effect::Mask(source)));
        }
        self.sources.set(source, Source::Masked(mode));
        Ok(())
    }

    /// Reconcile the original source mask from native enable readback.
    pub(super) fn confirm_mask(
        &mut self,
        context: &mut Context<'_>,
        source: u32,
    ) -> Result<(), Failure> {
        self.check(context)?;
        if self.state != State::Uncertain(Effect::Mask(source)) {
            return Err(Failure::Busy);
        }
        let Source::Masking(mode) = self.source(source)? else {
            return Err(Failure::Busy);
        };
        if self.registers.enabled(source) {
            return Err(Failure::Unconfirmed);
        }
        // Undo only the local safety hold imposed while the mask was unknown.
        if !context.enabled() {
            context.set_enabled(true);
            if !context.enabled() {
                return Err(Failure::Unconfirmed);
            }
        }
        self.sources.set(source, Source::Masked(mode));
        self.state = State::Open;
        Ok(())
    }

    /// # Safety
    /// Before enabling global SIE, install a trusted reception path with
    /// capacity for the original exclusive claim slot.
    pub unsafe fn open(&mut self, context: &mut Context<'_>) -> Result<(), Failure> {
        self.quiet(context)?;
        if self.state != State::Prepared {
            return Err(Failure::Busy);
        }
        self.state = State::Opening;
        context.open();
        let configuration = DomainConfiguration::open();
        self.registers.configure(configuration);
        if !self.registers.confirms(configuration)
            || !context.delivery(true)
            || context.threshold() != 0
        {
            return Err(self.unconfirmed(context, Effect::Opening));
        }
        context.set_enabled(true);
        if !context.enabled() {
            return Err(self.unconfirmed(context, Effect::Opening));
        }
        self.state = State::Open;
        Ok(())
    }

    /// Observe the source associated with an exact outstanding IDC claim.
    /// Direct delivery has no Complete write. Preserve later edge pending.
    pub(super) fn settle_source(
        &mut self,
        context: &mut Context<'_>,
        source: u32,
    ) -> Result<(), Failure> {
        let Source::Masked(mode) = self.source(source)? else {
            return Err(Failure::Busy);
        };
        if self.registers.enabled(source) {
            self.sources.set(source, Source::Masking(mode));
            return Err(self.unconfirmed(context, Effect::Mask(source)));
        }
        if matches!(mode, Mode::LevelHigh | Mode::LevelLow)
            && (self.registers.input(source) || self.registers.pending(source))
        {
            return Err(Failure::Busy);
        }
        Ok(())
    }

    /// # Safety
    /// All original devices have stopped their old events and the caller
    /// excludes hardware effects that could still reach this domain or IDC.
    pub unsafe fn shut(&mut self, context: &mut Context<'_>) -> Result<(), Failure> {
        self.quiet(context)?;
        match self.state {
            State::Shut => return Ok(()),
            State::Prepared | State::Open => {
                self.state = State::Shutting;
                context.set_enabled(false);
                let configuration = DomainConfiguration::shut();
                self.registers.configure(configuration);
                context.shut();
                if context.enabled()
                    || !self.registers.confirms(configuration)
                    || !context.delivery(false)
                {
                    return Err(self.unconfirmed(context, Effect::ShutDelivery));
                }
            }
            State::Shutting => {}
            _ => return Err(Failure::Busy),
        }

        // Mask every source before pending evidence can suspend retirement.
        for source in 1..=self.description.sources() {
            if let Source::Enabled(mode) = self.source(source)? {
                self.mask(context, source, mode)?;
            }
        }
        for source in 1..=self.description.sources() {
            if let Source::Masked(mode) = self.source(source)? {
                if self.registers.enabled(source) {
                    self.sources.set(source, Source::Masking(mode));
                    return Err(self.unconfirmed(context, Effect::Mask(source)));
                }
                if self.registers.pending(source)
                    || (matches!(mode, Mode::LevelHigh | Mode::LevelLow)
                        && self.registers.input(source))
                {
                    return Err(Failure::Busy);
                }
            } else if self.source(source)? != Source::Vacant {
                return Err(Failure::Busy);
            }
        }
        for source in 1..=self.description.sources() {
            if let Source::Masked(mode) = self.source(source)? {
                self.sources.set(source, Source::Retiring(mode));
                let configuration = SourceConfiguration::Inactive;
                self.registers.set_source(source, configuration);
                if !configuration.confirms(self.registers.source(source)) {
                    return Err(self.unconfirmed(context, Effect::Retire(source)));
                }
                self.sources.set(source, Source::Vacant);
            }
        }
        self.state = State::Shut;
        Ok(())
    }
}
