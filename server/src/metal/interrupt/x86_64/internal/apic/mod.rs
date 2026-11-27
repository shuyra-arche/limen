//! The exclusive local xAPIC mapping and mechanism admission.

use limen_treaty::model::Alignment;

use crate::metal::{
    memory::access::treaty::device::DeviceWindow, platform::description::acpi::Madt,
};

pub mod launch;
mod registers;
use registers::{Base, DESTINATION_SHIFT, ID, Icr, PAGE, VERSION, integrated};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Failure {
    Scope,
    Busy,
    Unconfirmed,
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

/// Exclusive local xAPIC register owner, including ICR and EOI.
/// Drop neither retires a device mapping nor settles an uncertain send.
#[must_use]
pub struct LocalApic<'map> {
    base: Base,
    hart: usize,
    window: DeviceWindow<'map>,
    probe: Probe,
    icr: Icr,
    reception: Reception,
    vectors: [u64; 4],
    timer: timer::Slot,
}

impl<'map> LocalApic<'map> {
    /// # Safety
    /// The actual caller has exclusive control of this Hart's local APIC,
    /// including both ICR halves; the supplied page is the live UC mapping.
    /// Retain all original effects and mapping through every use or failure.
    /// No other entry may conflict with this controller during the sequence.
    /// Before readmitting a controller, settle all prior vector leases.
    pub unsafe fn admit(
        hart: usize,
        madt: Madt<'_>,
        mut window: DeviceWindow<'map>,
        probe: Probe,
    ) -> Result<Self, (DeviceWindow<'map>, Failure)> {
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
            icr: Icr::Idle,
            reception: Reception::Idle,
            vectors: [0; 4],
            timer: timer::Slot::Unobserved,
        })
    }
}

mod reception;
pub use reception::{Claim, Reception, VectorLease};
mod timer;
pub use timer::{Countdown, Divider, TimerState};

mod ipi;
pub use ipi::{IpiRoute, IpiSend, IpiTarget};

#[cfg(all(limen_target, target_arch = "x86_64"))]
pub(in crate::metal) mod native;

#[cfg(test)]
#[path = "../../../../../../tests/metal/interrupt/x86_64/internal/apic/mod.rs"]
mod tests;
