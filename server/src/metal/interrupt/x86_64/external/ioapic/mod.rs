//! Legacy physical fixed-vector routing through an exclusively held I/O APIC port.

use crate::metal::{
    interrupt::x86_64::internal::apic::VectorLease,
    memory::mmu::access::device::DeviceWindow,
    platform::description::acpi::{IoApic, Madt, Polarity, Trigger},
};

const SELECT: usize = 0;
const DATA: usize = 0x10;
const FIRST_ENTRY: u8 = 0x10;
const PIN_LIMIT: usize = (256 - FIRST_ENTRY as usize) / 2;
const MASKED: u32 = 1 << 16;
const LEVEL: u32 = 1 << 15;
const REMOTE_IRR: u32 = 1 << 14;
const ACTIVE_LOW: u32 = 1 << 13;
const SENDING: u32 = 1 << 12;
const PROGRAMMABLE: u32 = MASKED | LEVEL | ACTIVE_LOW | 0xfff;
const DESTINATION: u32 = 0xff00_0000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Failure {
    Scope,
    Busy,
    Unconfirmed,
}

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
    fn low(self, masked: bool) -> u32 {
        u32::from(self.vector)
            | if masked { MASKED } else { 0 }
            | if self.level { LEVEL } else { 0 }
            | if self.active_low { ACTIVE_LOW } else { 0 }
    }

    fn high(self) -> u32 {
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

struct Port<'a>(DeviceWindow<'a>);

impl Port<'_> {
    fn select(&mut self, register: u8) {
        // The caller excludes every other user of this shared selector/window pair.
        unsafe { self.0.write_u32(SELECT, u32::from(register)) }.expect("I/O APIC selector");
    }

    fn read(&mut self, register: u8) -> u32 {
        self.select(register);
        unsafe { self.0.read_u32(DATA) }.expect("I/O APIC register")
    }

    fn write(&mut self, register: u8, value: u32) {
        self.select(register);
        unsafe { self.0.write_u32(DATA, value) }.expect("I/O APIC register");
    }
}

#[derive(Clone, Copy)]
struct Pins {
    first: u32,
    last: u8,
}

impl Pins {
    fn probe(first: u32, version: u32) -> Option<Self> {
        // 82093AA/equivalent direct-format window, including QEMU's 0x20 profile.
        let last = (version >> 16) as u8;
        if !matches!(version as u8, 0x11 | 0x20) || usize::from(last) >= PIN_LIMIT {
            return None;
        }
        first.checked_add(u32::from(last))?;
        Some(Self { first, last })
    }

    fn pin(self, gsi: u32) -> Result<u8, Failure> {
        let pin = gsi
            .checked_sub(self.first)
            .and_then(|n| u8::try_from(n).ok());
        pin.filter(|&n| n <= self.last).ok_or(Failure::Scope)
    }
}

/// One actual port and its per-pin programming obligations. Dropping the
/// object neither disables hardware nor proves drain.
#[must_use]
pub struct Controller<'a> {
    port: Port<'a>,
    madt: Madt<'a>,
    pins: Pins,
    states: [State; PIN_LIMIT],
    leases: [Option<VectorLease>; PIN_LIMIT],
}

impl<'a> Controller<'a> {
    /// # Safety
    /// The window is this physical controller's live, UC, x86 mapping. The
    /// caller retains exclusive access to its selector and data pair across
    /// CPUs and interrupt contexts, including failures. Direct compatibility
    /// format is active, interrupt remapping cannot reinterpret these entries,
    /// and this controller's probed GSI interval is exclusive in the system.
    pub unsafe fn admit(madt: Madt<'a>, window: &'a mut DeviceWindow<'_>) -> Result<Self, Failure> {
        let address = window.physical().start();
        let IoApic { id, gsi_base, .. } = madt
            .io_apics()
            .find(|item| item.address == address)
            .ok_or(Failure::Scope)?;
        if !address.get().is_multiple_of(4096)
            || !window.supports_u32(SELECT)
            || !window.supports_u32(DATA)
        {
            return Err(Failure::Scope);
        }
        let mut port = Port(window.reborrow());
        if id > 15 || (port.read(0) >> 24) != u32::from(id) {
            return Err(Failure::Scope);
        }
        let pins = Pins::probe(gsi_base, port.read(1)).ok_or(Failure::Scope)?;
        if madt
            .io_apics()
            .any(|item| item.id != id && pins.pin(item.gsi_base).is_ok())
        {
            return Err(Failure::Scope);
        }
        Ok(Self {
            port,
            madt,
            pins,
            states: [State::Vacant; PIN_LIMIT],
            leases: [const { None }; PIN_LIMIT],
        })
    }

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

    fn validate(&self, route: Route) -> Result<u8, Failure> {
        // The common physical-destination subset excludes the legacy broadcast ID.
        if route.hart >= 15 || route.vector < 0x20 || !self.madt.enabled_local_apic(route.hart) {
            return Err(Failure::Scope);
        }
        self.pins.pin(route.gsi)
    }

    pub fn state(&self, gsi: u32) -> Result<State, Failure> {
        Ok(self.states[usize::from(self.pins.pin(gsi)?)])
    }

    /// Attach the receiving Local APIC's lease before any selector writes.
    pub fn reserve(
        &mut self,
        route: Route,
        lease: VectorLease,
    ) -> Result<(), (VectorLease, Failure)> {
        let pin = match self.validate(route) {
            Ok(pin) if lease.matches(route.hart, route.vector) => usize::from(pin),
            _ => return Err((lease, Failure::Scope)),
        };
        if self.states[pin] != State::Vacant {
            return Err((lease, Failure::Busy));
        }
        self.leases[pin] = Some(lease);
        self.states[pin] = State::Reserved(route);
        Ok(())
    }

    /// Return a reservation that has issued no hardware operation.
    pub fn cancel(&mut self, gsi: u32) -> Result<VectorLease, Failure> {
        let pin = usize::from(self.pins.pin(gsi)?);
        if !matches!(self.states[pin], State::Reserved(_)) {
            return Err(Failure::Busy);
        }
        self.release(pin)
    }

    fn program(&mut self, pin: u8, route: Route, effect: Effect) -> Result<(), Failure> {
        let register = FIRST_ENTRY + pin * 2;
        let masked = effect != Effect::Enable;
        let low = route.low(masked);
        let state = &mut self.states[usize::from(pin)];
        *state = State::Unconfirmed(route, effect);
        if effect == Effect::Configure {
            self.port.write(register + 1, route.high());
            if self.port.read(register + 1) & DESTINATION != route.high() {
                return Err(Failure::Unconfirmed);
            }
        }
        self.port.write(register, low);
        if self.port.read(register) & PROGRAMMABLE != low {
            return Err(Failure::Unconfirmed);
        }
        *state = if masked {
            State::Masked(route)
        } else {
            State::Enabled(route)
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
        let index = usize::from(pin);
        let available = match self.states[index] {
            State::Reserved(original) | State::Unconfirmed(original, Effect::Configure) => {
                original == route
            }
            _ => false,
        };
        if !available
            || self.port.read(FIRST_ENTRY + pin * 2) & (MASKED | SENDING | REMOTE_IRR) != MASKED
        {
            return Err(Failure::Busy);
        }
        self.program(pin, route, Effect::Configure)
    }

    /// # Safety
    /// The original target has a live vector, reserved reception capacity
    /// and the matching Local APIC EOI path. Level delivery requires EOI
    /// broadcasts or an independently verified directed EOI path. The device
    /// and its handler are ready, including any necessary rearm after service.
    pub unsafe fn enable(&mut self, gsi: u32) -> Result<(), Failure> {
        let pin = self.pins.pin(gsi)?;
        let State::Masked(route) = self.states[usize::from(pin)] else {
            return Err(Failure::Busy);
        };
        self.program(pin, route, Effect::Enable)
    }

    /// Confirm masking of future input. Already accepted delivery can persist.
    pub fn mask(&mut self, gsi: u32) -> Result<(), Failure> {
        let pin = self.pins.pin(gsi)?;
        let route = match self.states[usize::from(pin)] {
            State::Enabled(route) | State::Unconfirmed(route, Effect::Enable | Effect::Mask) => {
                route
            }
            _ => return Err(Failure::Busy),
        };
        // A previous enable/mask may already have taken effect. Both retain
        // the same confirmed fixed route; a final mask write is idempotent.
        self.program(pin, route, Effect::Mask)
    }

    /// # Safety
    /// The original device, messages in flight, receiving Local APIC and
    /// software reception have all been drained independently. The observed
    /// mask, send status and Remote IRR only describe this controller.
    pub unsafe fn retire(&mut self, gsi: u32) -> Result<VectorLease, Failure> {
        let pin = self.pins.pin(gsi)?;
        let State::Masked(_) = self.states[usize::from(pin)] else {
            return Err(Failure::Busy);
        };
        if self.port.read(FIRST_ENTRY + pin * 2) & (MASKED | SENDING | REMOTE_IRR) != MASKED {
            return Err(Failure::Busy);
        }
        self.release(usize::from(pin))
    }

    fn release(&mut self, pin: usize) -> Result<VectorLease, Failure> {
        let lease = self.leases[pin].take().ok_or(Failure::Busy)?;
        self.states[pin] = State::Vacant;
        Ok(lease)
    }
}

#[cfg(test)]
#[path = "../../../../../../tests/metal/interrupt/x86_64/external/ioapic/mod.rs"]
mod tests;
