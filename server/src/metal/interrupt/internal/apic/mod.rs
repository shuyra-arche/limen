//! Actual local xAPIC startup control, separate from the Boot trampoline.

use limen_treaty::model::{Alignment, ByteLength, PhysicalAddress, PhysicalRange};

use crate::metal::{memory::mmu::internal::device::DeviceWindow, platform::description::acpi::Madt};

const PAGE: u64 = 4096;
const ADDRESS: u64 = 0x000f_ffff_ffff_f000;
const BSP: u64 = 1 << 8;
const X2APIC: u64 = 1 << 10;
const ENABLE: u64 = 1 << 11;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Base {
    physical: PhysicalRange,
    major: bool,
}

impl Base {
    fn decode(bits: u64) -> Option<Self> {
        // Only the enabled legacy memory-mapped mode is implemented.
        if bits & (ENABLE | X2APIC) != ENABLE || bits & !(ADDRESS | BSP | X2APIC | ENABLE) != 0 {
            return None;
        }
        let address = bits & ADDRESS;
        if address == 0 {
            return None;
        }
        Some(Self {
            physical: PhysicalAddress::new(address).range(ByteLength::new(PAGE))?,
            major: bits & BSP != 0,
        })
    }

    fn matches(self, hart: usize, madt: Madt<'_>) -> bool {
        self.major
            && madt.local_controller_address() == Some(self.physical.start())
            && Destination::new(hart).is_some_and(|id| madt.enabled_local_apic(id.0))
    }
}

const DESTINATION_SHIFT: u32 = 24;
const DELIVERY_INIT: u32 = 5 << 8;
const DELIVERY_STARTUP: u32 = 6 << 8;
const SEND_PENDING: u32 = 1 << 12;
const LEVEL_ASSERT: u32 = 1 << 14;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Destination(u8);

impl Destination {
    fn new(hart: usize) -> Option<Self> {
        let id = u8::try_from(hart).ok()?;
        (id != u8::MAX).then_some(Self(id))
    }

    fn bits(self) -> u32 {
        u32::from(self.0) << DESTINATION_SHIFT
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Vector(u8);

impl Vector {
    fn new(page: PhysicalAddress) -> Option<Self> {
        let address = page.get();
        if address == 0 || !address.is_multiple_of(PAGE) {
            return None;
        }
        u8::try_from(address / PAGE).ok().map(Self)
    }
}

enum Command {
    Init,
    Startup(Vector),
}

impl Command {
    /// Explicit physical destination and edge trigger; never echo ICR status.
    fn bits(self) -> u32 {
        match self {
            Self::Init => LEVEL_ASSERT | DELIVERY_INIT,
            Self::Startup(vector) => LEVEL_ASSERT | DELIVERY_STARTUP | u32::from(vector.0),
        }
    }
}

fn idle(bits: u32) -> bool {
    bits & SEND_PENDING == 0
}

fn integrated(version: u32) -> bool {
    (0x10..=0x15).contains(&(version & 0xff))
}

const ID: usize = 0x20;
const VERSION: usize = 0x30;
const ICR_LOW: usize = 0x300;
const ICR_HIGH: usize = 0x310;
const POLLS: usize = 100_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Failure {
    Scope,
    Busy,
    Unconfirmed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Stage {
    Init,
    First,
    Second,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Effect {
    pub hart: u8,
    pub page: PhysicalAddress,
    pub stage: Stage,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum State {
    Idle,
    Sending(Effect),
    Unconfirmed(Effect),
}

/// Fresh facts from the actual ordinary local CPU environment.
struct Observation {
    bits: u64,
    hart: usize,
}

/// A concrete native observation source, not a mapping or ICR access permit.
/// Its constructor is supplied only by the actual native implementation.
#[derive(Clone, Copy)]
pub struct Probe {
    read: fn() -> Option<Observation>,
}

impl Probe {
    fn base_for(self, hart: usize) -> Option<Base> {
        let observed = (self.read)()?;
        if observed.hart != hart {
            return None;
        }
        Base::decode(observed.bits)
    }
}

/// The actual exclusive local ICR owner, not a firmware description.
/// Drop neither retires a device mapping nor settles an uncertain send.
#[must_use]
pub struct LocalApic {
    base: Base,
    hart: usize,
    window: DeviceWindow,
    probe: Probe,
    state: State,
}

impl LocalApic {
    /// # Safety
    /// The actual caller has exclusive control of this Hart's local APIC,
    /// including both ICR halves; the supplied page is the live UC mapping.
    /// Retain all original effects and mapping through every use or failure.
    /// No other entry may conflict with this controller during the sequence.
    pub unsafe fn admit(
        hart: usize,
        madt: Madt<'_>,
        mut window: DeviceWindow,
        probe: Probe,
    ) -> Result<Self, (DeviceWindow, Failure)> {
        let Some(base) = probe.base_for(hart) else {
            return Err((window, Failure::Scope));
        };
        if !base.matches(hart, madt)
            || window.physical() != base.physical
            || !window.is_aligned(Alignment::new(PAGE).expect("APIC page alignment"))
        {
            return Err((window, Failure::Scope));
        }
        // These are non-destructive read-only hardware observations.
        let id = unsafe { window.read_u32(ID) }.expect("APIC ID geometry");
        let version = unsafe { window.read_u32(VERSION) }.expect("APIC version geometry");
        if id >> DESTINATION_SHIFT != hart as u32 || !integrated(version) {
            return Err((window, Failure::Scope));
        }
        Ok(Self {
            base,
            hart,
            window,
            probe,
            state: State::Idle,
        })
    }

    pub fn state(&self) -> State {
        self.state
    }

    /// Pure target geometry and original scope, not live startup authority.
    pub fn can_start(&self, hart: usize) -> bool {
        hart != self.hart && Destination::new(hart).is_some()
    }

    /// Recheck the actual current native environment and retained effect
    /// before preparing or rewriting the trampoline. This alone grants no
    /// authority to send, and the start operation checks again.
    pub fn preflight(&self, hart: usize, page: PhysicalAddress) -> Result<(), Failure> {
        if !self.can_start(hart)
            || Vector::new(page).is_none()
            || self.probe.base_for(self.hart) != Some(self.base)
        {
            return Err(Failure::Scope);
        }
        if self.state != State::Idle {
            return Err(Failure::Busy);
        }
        Ok(())
    }

    fn idle(&mut self) -> bool {
        for _ in 0..POLLS {
            if idle(unsafe { self.window.read_u32(ICR_LOW) }.expect("ICR geometry")) {
                return true;
            }
            core::hint::spin_loop();
        }
        false
    }

    fn send(&mut self, effect: Effect, destination: Destination, command: Command) {
        self.state = State::Sending(effect);
        // The UC mapping and exclusive ICR ownership order the upper half
        // before the lower write that causes the actual send.
        unsafe {
            self.window
                .write_u32(ICR_HIGH, destination.bits())
                .expect("ICR destination geometry");
            self.window
                .write_u32(ICR_LOW, command.bits())
                .expect("ICR command geometry");
        }
    }

    /// # Safety
    /// The caller owns the exact inactive target, published entry and its
    /// physical trampoline. All inputs remain valid until target arrival
    /// and all old accesses settle. The delay keeps the trusted Hart and
    /// masked state, never reenters ICR control, and reports a full interval.
    /// An uncertain sequence must retain this owner and its mapping.
    pub unsafe fn start(
        &mut self,
        hart: usize,
        page: PhysicalAddress,
        delay: fn(u32) -> bool,
    ) -> Result<(), Failure> {
        self.preflight(hart, page)?;
        let destination = Destination::new(hart).expect("checked destination");
        let vector = Vector::new(page).expect("checked vector");
        if !self.idle() || !delay(1) {
            return Err(Failure::Busy);
        }
        for (stage, command, interval) in [
            (Stage::Init, Command::Init, 10_000),
            (Stage::First, Command::Startup(vector), 200),
            (Stage::Second, Command::Startup(vector), 200),
        ] {
            let effect = Effect {
                hart: destination.0,
                page,
                stage,
            };
            self.send(effect, destination, command);
            if !self.idle() || !delay(interval) {
                self.state = State::Unconfirmed(effect);
                return Err(Failure::Unconfirmed);
            }
        }
        // Only the local send sequence is settled; the AP's actual entry and
        // continued ownership of the trampoline remain Boot responsibilities.
        self.state = State::Idle;
        Ok(())
    }
}

#[cfg(all(limen_target, target_arch = "x86_64"))]
pub(in crate::metal) mod native;

#[cfg(test)]
#[path = "../../../../../tests/metal/interrupt/internal/apic/mod.rs"]
mod tests;
