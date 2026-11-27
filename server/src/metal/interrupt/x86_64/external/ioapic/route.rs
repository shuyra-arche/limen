//! Route encoding and progress for each occupied physical pin.

use crate::metal::{
    interrupt::{
        treaty::{RouteControl, RouteOperation, RouteState},
        x86_64::internal::apic::VectorLease,
    },
    platform::description::acpi::{Polarity, Trigger},
};
use limen_treaty::model::PhysicalAddress;

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
    Publish,
    Resume,
    Mask,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum State {
    Vacant,
    Reserved(Route),
    Prepared(Route),
    Masked(Route),
    Enabled(Route),
    /// Readback failed after an effect. The original route remains occupied.
    Unconfirmed(Route, Effect),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    Reserved,
    Prepared,
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
            Phase::Prepared => State::Prepared(self.route),
            Phase::Masked => State::Masked(self.route),
            Phase::Enabled => State::Enabled(self.route),
            Phase::Unconfirmed(effect) => State::Unconfirmed(self.route, effect),
        }
    }
}

/// The exclusive reference to a pin and its original receiving vector.
#[must_use]
pub struct BoundRoute {
    controller: PhysicalAddress,
    route: Option<Route>,
}

struct Control<'route, 'map> {
    owner: &'route mut Controller<'map>,
    binding: &'route mut BoundRoute,
    pin: Pin,
}

impl crate::metal::interrupt::treaty::route::Sealed for Control<'_, '_> {}

impl RouteControl for Control<'_, '_> {
    type Failure = Failure;
    type Retired = VectorLease;

    fn state(&self) -> RouteState {
        match self.owner.routes[self.pin.index()]
            .as_ref()
            .map(|binding| binding.phase)
        {
            Some(Phase::Prepared) => RouteState::Prepared,
            Some(Phase::Enabled) => RouteState::Open,
            Some(Phase::Masked) => RouteState::Masked,
            Some(Phase::Unconfirmed(Effect::Publish)) => {
                RouteState::Unconfirmed(RouteOperation::Publish)
            }
            Some(Phase::Unconfirmed(Effect::Resume)) => {
                RouteState::Unconfirmed(RouteOperation::Resume)
            }
            Some(Phase::Unconfirmed(Effect::Mask)) => RouteState::Unconfirmed(RouteOperation::Mask),
            Some(Phase::Reserved | Phase::Unconfirmed(Effect::Configure)) => {
                unreachable!("I/O APIC control requires a configured route")
            }
            None => RouteState::Retired,
        }
    }

    unsafe fn publish(&mut self) -> Result<(), Failure> {
        unsafe {
            self.owner
                .enable(self.pin, Phase::Prepared, Effect::Publish)
        }
    }

    fn mask(&mut self) -> Result<(), Failure> {
        self.owner.mask(self.pin)
    }

    unsafe fn resume(&mut self) -> Result<(), Failure> {
        unsafe { self.owner.enable(self.pin, Phase::Masked, Effect::Resume) }
    }

    unsafe fn retire(&mut self) -> Result<VectorLease, Failure> {
        unsafe { self.owner.retire(self.pin, self.binding) }
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
    ) -> Result<BoundRoute, (VectorLease, Failure)> {
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
        Ok(BoundRoute {
            controller: self.port.address(),
            route: Some(route),
        })
    }

    fn bound_pin(&self, binding: &BoundRoute) -> Result<Pin, Failure> {
        let route = binding.route.ok_or(Failure::Scope)?;
        let pin = self.pins.pin(route.gsi)?;
        if binding.controller != self.port.address() || self.binding(pin)?.route != route {
            return Err(Failure::Scope);
        }
        Ok(pin)
    }

    /// Recover control of the original configured binding.
    pub fn control<'route>(
        &'route mut self,
        binding: &'route mut BoundRoute,
    ) -> Result<impl RouteControl<Failure = Failure, Retired = VectorLease> + 'route, Failure> {
        let pin = self.bound_pin(binding)?;
        if matches!(
            self.binding(pin)?.phase,
            Phase::Reserved | Phase::Unconfirmed(Effect::Configure)
        ) {
            return Err(Failure::Busy);
        }
        Ok(Control {
            owner: self,
            binding,
            pin,
        })
    }

    /// Return a reservation that has issued no hardware operation.
    pub fn cancel(&mut self, binding: &mut BoundRoute) -> Result<VectorLease, Failure> {
        let pin = self.bound_pin(binding)?;
        if self.binding(pin)?.phase != Phase::Reserved {
            return Err(Failure::Busy);
        }
        self.release(pin, binding)
    }

    fn program(&mut self, pin: Pin, effect: Effect) -> Result<(), Failure> {
        let binding = self.routes[pin.index()]
            .as_mut()
            .expect("reserved I/O APIC pin");
        let route = binding.route;
        let masked = matches!(effect, Effect::Configure | Effect::Mask);
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
        binding.phase = match effect {
            Effect::Configure => Phase::Prepared,
            Effect::Mask => Phase::Masked,
            Effect::Publish | Effect::Resume => Phase::Enabled,
        };
        Ok(())
    }

    /// # Safety
    /// This caller owns the exact source and its old route, has stopped and
    /// drained old delivery at the device and all prior destinations.
    /// Preserve these facts through retirement and exclude firmware PIC and NMI use.
    /// After unconfirmed readback, retry only the original masked route.
    pub unsafe fn configure<'route>(
        &'route mut self,
        binding: &'route mut BoundRoute,
    ) -> Result<impl RouteControl<Failure = Failure, Retired = VectorLease> + 'route, Failure> {
        let pin = self.bound_pin(binding)?;
        if !matches!(
            self.binding(pin)?.phase,
            Phase::Reserved | Phase::Unconfirmed(Effect::Configure)
        ) || !quiesced(self.port.read(pin.low_register()))
        {
            return Err(Failure::Busy);
        }
        self.program(pin, Effect::Configure)?;
        self.control(binding)
    }

    /// # Safety
    /// The original target has a live vector, reserved reception capacity
    /// and the matching Local APIC EOI path. Level delivery requires EOI
    /// broadcasts or an independently verified directed EOI path. The device
    /// and its handler are ready, including any necessary rearm after service.
    unsafe fn enable(&mut self, pin: Pin, ready: Phase, effect: Effect) -> Result<(), Failure> {
        let phase = self.binding(pin)?.phase;
        if phase != ready && phase != Phase::Unconfirmed(effect) {
            return Err(Failure::Busy);
        }
        self.program(pin, effect)
    }

    /// Confirm masking of future input. Already accepted delivery can persist.
    fn mask(&mut self, pin: Pin) -> Result<(), Failure> {
        match self.binding(pin)?.phase {
            Phase::Masked => return Ok(()),
            Phase::Enabled
            | Phase::Unconfirmed(Effect::Publish | Effect::Resume | Effect::Mask) => {}
            _ => return Err(Failure::Busy),
        }
        // A previous enable/mask may already have taken effect. Both retain
        // the same confirmed fixed route; a final mask write is idempotent.
        self.program(pin, Effect::Mask)
    }

    /// # Safety
    /// The original device, messages in flight, receiving Local APIC and
    /// software reception have all been drained independently. The observed
    /// mask, send status and Remote IRR only describe this controller.
    unsafe fn retire(
        &mut self,
        pin: Pin,
        binding: &mut BoundRoute,
    ) -> Result<VectorLease, Failure> {
        if !matches!(self.binding(pin)?.phase, Phase::Prepared | Phase::Masked)
            || !quiesced(self.port.read(pin.low_register()))
        {
            return Err(Failure::Busy);
        }
        self.release(pin, binding)
    }

    fn binding(&self, pin: Pin) -> Result<&Binding, Failure> {
        self.routes[pin.index()].as_ref().ok_or(Failure::Busy)
    }

    fn release(&mut self, pin: Pin, binding: &mut BoundRoute) -> Result<VectorLease, Failure> {
        let original = self.routes[pin.index()].take().ok_or(Failure::Busy)?;
        binding.route = None;
        Ok(original.vector)
    }
}

pub(super) fn quiesced(low: u32) -> bool {
    low & (MASKED | SENDING | REMOTE_IRR) == MASKED
}
