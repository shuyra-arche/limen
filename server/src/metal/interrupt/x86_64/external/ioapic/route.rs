//! Route encoding and progress for each occupied physical pin.

use crate::metal::{
    interrupt::x86_64::internal::apic::VectorLease,
    platform::description::acpi::{Polarity, Trigger},
};

use super::{Controller, Failure, pin::Pin};

const MASKED: u32 = 1 << 16;
const LEVEL: u32 = 1 << 15;
const REMOTE_IRR: u32 = 1 << 14;
const ACTIVE_LOW: u32 = 1 << 13;
const SENDING: u32 = 1 << 12;
const PROGRAMMABLE: u32 = MASKED | LEVEL | ACTIVE_LOW | 0xfff;
const DESTINATION: u32 = 0xff00_0000;

/// Physical fixed delivery to a reserved vector on the receiving Hart.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Route {
    pub gsi: u32,
    pub hart: u8,
    pub vector: u8,
    pub active_low: bool,
    pub level: bool,
}

impl Route {
    pub(super) fn low(self, masked: bool) -> u32 {
        u32::from(self.vector)
            | if masked { MASKED } else { 0 }
            | if self.level { LEVEL } else { 0 }
            | if self.active_low { ACTIVE_LOW } else { 0 }
    }

    pub(super) fn high(self) -> u32 {
        u32::from(self.hart) << 24
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Effect {
    Configure,
    Enable,
    Mask,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum State {
    Vacant,
    Reserved(Route),
    Masked(Route),
    Enabled(Route),
    /// Readback failed after an effect. The original route remains occupied.
    Unconfirmed(Route, Effect),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    Reserved,
    Masked,
    Enabled,
    Unconfirmed(Effect),
}

/// The sole route state retains the lease from the receiving Local APIC.
pub(super) struct Binding {
    route: Route,
    vector: VectorLease,
    phase: Phase,
}

impl Binding {
    fn observe(&self) -> State {
        match self.phase {
            Phase::Reserved => State::Reserved(self.route),
            Phase::Masked => State::Masked(self.route),
            Phase::Enabled => State::Enabled(self.route),
            Phase::Unconfirmed(effect) => State::Unconfirmed(self.route, effect),
        }
    }
}

impl Controller<'_> {
    /// Resolve an ISA input, including a MADT source override. Conforming ISA
    /// routes are active-high, edge-triggered. An overridden GSI has no second
    /// implicit identity route for the old IRQ at that number.
    pub fn isa_route(&self, irq: u8, hart: u8, vector: u8) -> Result<Route, Failure> {
        if irq >= 16 {
            return Err(Failure::Scope);
        }
        let mut route = Route {
            gsi: u32::from(irq),
            hart,
            vector,
            active_low: false,
            level: false,
        };
        if let Some(input) = self
            .madt
            .source_overrides()
            .find(|item| item.isa_irq == irq)
        {
            route.gsi = input.gsi;
            route.level = input.trigger == Trigger::Level;
            route.active_low = input.polarity == Polarity::ActiveLow;
        }
        if self
            .madt
            .source_overrides()
            .any(|item| item.gsi == route.gsi && item.isa_irq != irq)
        {
            return Err(Failure::Scope);
        }
        self.validate(route)?;
        Ok(route)
    }

    fn validate(&self, route: Route) -> Result<Pin, Failure> {
        // The common physical-destination subset excludes the legacy broadcast ID.
        if route.hart >= 15 || route.vector < 0x20 || !self.madt.enabled_local_apic(route.hart) {
            return Err(Failure::Scope);
        }
        self.pins.pin(route.gsi)
    }

    pub fn state(&self, gsi: u32) -> Result<State, Failure> {
        let pin = self.pins.pin(gsi)?;
        Ok(self.routes[pin.index()]
            .as_ref()
            .map_or(State::Vacant, Binding::observe))
    }

    /// Attach the receiving Local APIC's lease before any selector writes.
    pub fn reserve(
        &mut self,
        route: Route,
        lease: VectorLease,
    ) -> Result<(), (VectorLease, Failure)> {
        let pin = match self.validate(route) {
            Ok(pin) if lease.matches(route.hart, route.vector) => pin,
            _ => return Err((lease, Failure::Scope)),
        };
        let original = &mut self.routes[pin.index()];
        if original.is_some() {
            return Err((lease, Failure::Busy));
        }
        *original = Some(Binding {
            route,
            vector: lease,
            phase: Phase::Reserved,
        });
        Ok(())
    }

    /// Return a reservation that has issued no hardware operation.
    pub fn cancel(&mut self, gsi: u32) -> Result<VectorLease, Failure> {
        let pin = self.pins.pin(gsi)?;
        if self.binding(pin)?.phase != Phase::Reserved {
            return Err(Failure::Busy);
        }
        self.release(pin)
    }

    fn program(&mut self, pin: Pin, effect: Effect) -> Result<(), Failure> {
        let binding = self.routes[pin.index()]
            .as_mut()
            .expect("reserved I/O APIC pin");
        let route = binding.route;
        let masked = effect != Effect::Enable;
        let low = route.low(masked);
        binding.phase = Phase::Unconfirmed(effect);
        if effect == Effect::Configure {
            let register = pin.high_register();
            self.port.write(register, route.high());
            if self.port.read(register) & DESTINATION != route.high() {
                return Err(Failure::Unconfirmed);
            }
        }
        let register = pin.low_register();
        self.port.write(register, low);
        if self.port.read(register) & PROGRAMMABLE != low {
            return Err(Failure::Unconfirmed);
        }
        binding.phase = if masked {
            Phase::Masked
        } else {
            Phase::Enabled
        };
        Ok(())
    }

    /// # Safety
    /// This caller owns the exact source and its old route, has stopped and
    /// drained old delivery at the device and all prior destinations.
    /// Preserve these facts through retirement and exclude firmware PIC and NMI use.
    /// After unconfirmed readback, retry only the original masked route.
    pub unsafe fn configure(&mut self, route: Route) -> Result<(), Failure> {
        let pin = self.validate(route)?;
        let binding = self.binding(pin)?;
        if binding.route != route
            || !matches!(
                binding.phase,
                Phase::Reserved | Phase::Unconfirmed(Effect::Configure)
            )
            || !quiesced(self.port.read(pin.low_register()))
        {
            return Err(Failure::Busy);
        }
        self.program(pin, Effect::Configure)
    }

    /// # Safety
    /// The original target has a live vector, reserved reception capacity
    /// and the matching Local APIC EOI path. Level delivery requires EOI
    /// broadcasts or an independently verified directed EOI path. The device
    /// and its handler are ready, including any necessary rearm after service.
    pub unsafe fn enable(&mut self, gsi: u32) -> Result<(), Failure> {
        let pin = self.pins.pin(gsi)?;
        if self.binding(pin)?.phase != Phase::Masked {
            return Err(Failure::Busy);
        }
        self.program(pin, Effect::Enable)
    }

    /// Confirm masking of future input. Already accepted delivery can persist.
    pub fn mask(&mut self, gsi: u32) -> Result<(), Failure> {
        let pin = self.pins.pin(gsi)?;
        if !matches!(
            self.binding(pin)?.phase,
            Phase::Enabled | Phase::Unconfirmed(Effect::Enable | Effect::Mask)
        ) {
            return Err(Failure::Busy);
        }
        // A previous enable/mask may already have taken effect. Both retain
        // the same confirmed fixed route; a final mask write is idempotent.
        self.program(pin, Effect::Mask)
    }

    /// # Safety
    /// The original device, messages in flight, receiving Local APIC and
    /// software reception have all been drained independently. The observed
    /// mask, send status and Remote IRR only describe this controller.
    pub unsafe fn retire(&mut self, gsi: u32) -> Result<VectorLease, Failure> {
        let pin = self.pins.pin(gsi)?;
        if self.binding(pin)?.phase != Phase::Masked
            || !quiesced(self.port.read(pin.low_register()))
        {
            return Err(Failure::Busy);
        }
        self.release(pin)
    }

    fn binding(&self, pin: Pin) -> Result<&Binding, Failure> {
        self.routes[pin.index()].as_ref().ok_or(Failure::Busy)
    }

    fn release(&mut self, pin: Pin) -> Result<VectorLease, Failure> {
        let binding = self.routes[pin.index()].take().ok_or(Failure::Busy)?;
        Ok(binding.vector)
    }
}

pub(super) fn quiesced(low: u32) -> bool {
    low & (MASKED | SENDING | REMOTE_IRR) == MASKED
}
