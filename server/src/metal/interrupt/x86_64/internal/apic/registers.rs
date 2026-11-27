//! Physical xAPIC register layout and the single ICR transaction.

use limen_treaty::model::{ByteLength, PhysicalAddress, PhysicalRange};

use crate::metal::platform::description::acpi::Madt;

use super::{LocalApic, ipi::IpiIssue, launch};

pub(super) const PAGE: u64 = 4096;
const ADDRESS: u64 = 0x000f_ffff_ffff_f000;
const BSP: u64 = 1 << 8;
const X2APIC: u64 = 1 << 10;
const ENABLE: u64 = 1 << 11;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Base {
    pub(super) physical: PhysicalRange,
    pub(super) major: bool,
}

impl Base {
    pub(super) fn decode(bits: u64) -> Option<Self> {
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

    pub(super) fn matches(self, processor: usize, madt: Madt<'_>) -> bool {
        madt.local_controller_address() == Some(self.physical.start())
            && Destination::new(processor).is_some_and(|id| madt.enabled_local_apic(id.0))
    }
}

pub(super) const DESTINATION_SHIFT: u32 = 24;
const SEND_PENDING: u32 = 1 << 12;
pub(super) const LEVEL_ASSERT: u32 = 1 << 14;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Destination(pub(super) u8);

impl Destination {
    pub(super) fn new(processor: usize) -> Option<Self> {
        let id = u8::try_from(processor).ok()?;
        (id != u8::MAX).then_some(Self(id))
    }

    pub(super) fn bits(self) -> u32 {
        u32::from(self.0) << DESTINATION_SHIFT
    }
}

pub(super) fn idle(bits: u32) -> bool {
    bits & SEND_PENDING == 0
}

pub(super) fn integrated(version: u32) -> bool {
    (0x10..=0x15).contains(&(version & 0xff))
}

pub(super) const ID: usize = 0x20;
pub(super) const VERSION: usize = 0x30;
const ICR_LOW: usize = 0x300;
const ICR_HIGH: usize = 0x310;
const POLLS: usize = 100_000;

/// Every issued command remains in the original controller until settled.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Icr {
    Idle,
    Launching(launch::Effect),
    LaunchUnconfirmed(launch::Effect),
    Notifying(IpiIssue),
}

impl Icr {
    pub(super) fn notification(self) -> Option<IpiIssue> {
        match self {
            Self::Notifying(issue) => Some(issue),
            _ => None,
        }
    }
}

impl LocalApic<'_> {
    pub(super) fn wait_icr(&mut self) -> bool {
        for _ in 0..POLLS {
            if idle(unsafe { self.window.read_u32(ICR_LOW) }.expect("ICR geometry")) {
                return true;
            }
            core::hint::spin_loop();
        }
        false
    }

    pub(super) fn write_icr(&mut self, destination: Destination, command: u32) {
        // The UC mapping and exclusive ICR ownership order the upper half
        // before the lower write that causes the actual send.
        unsafe {
            self.window
                .write_u32(ICR_HIGH, destination.bits())
                .expect("ICR destination geometry");
            self.window
                .write_u32(ICR_LOW, command)
                .expect("ICR command geometry");
        }
    }
}
