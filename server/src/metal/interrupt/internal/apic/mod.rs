//! Actual local xAPIC startup control, separate from the Boot trampoline.

use limen_treaty::model::{ByteLength, PhysicalAddress, PhysicalRange};

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

#[cfg(all(limen_target, target_arch = "x86_64"))]
mod native;
#[cfg(all(limen_target, target_arch = "x86_64"))]
pub(crate) use native::matches_madt;
#[cfg(all(limen_target, target_arch = "x86_64"))]
pub use native::{Effect, Failure, LocalApic, Stage, State};

#[cfg(test)]
#[path = "../../../../../tests/metal/interrupt/internal/apic/mod.rs"]
mod tests;
