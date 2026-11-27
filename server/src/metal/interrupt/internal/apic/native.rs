use core::arch::{asm, x86_64::__cpuid};

use limen_treaty::model::{Alignment, PhysicalAddress};

use crate::metal::{
    memory::mmu::internal::device::DeviceWindow,
    platform::description::acpi::Madt,
    processor::hart::{local, x86_64},
};

use super::{Base, Command, Destination, PAGE, Vector, idle, integrated};

const APIC_BASE: u32 = 0x1b;
const FEATURE_MSR: u32 = 1 << 5;
const FEATURE_APIC: u32 = 1 << 9;
const ID: usize = 0x20;
const VERSION: usize = 0x30;
const ICR_LOW: usize = 0x300;
const ICR_HIGH: usize = 0x310;
const INTERRUPT_ENABLE: usize = 1 << 9;
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

fn base() -> Option<Base> {
    let cs: u16;
    unsafe { asm!("mov {:x}, cs", out(reg) cs, options(nomem, nostack, preserves_flags)) };
    if cs & 3 != 0 {
        return None;
    }
    let features = __cpuid(1);
    if features.edx & (FEATURE_MSR | FEATURE_APIC) != (FEATURE_MSR | FEATURE_APIC) {
        return None;
    }
    let (low, high): (u32, u32);
    unsafe {
        asm!("rdmsr", in("ecx") APIC_BASE, out("eax") low, out("edx") high,
            options(nomem, nostack, preserves_flags));
    }
    Base::decode((u64::from(high) << 32) | u64::from(low))
}

fn masked() -> bool {
    let flags: usize;
    unsafe { asm!("pushfq", "pop {}", out(reg) flags, options(preserves_flags)) };
    flags & INTERRUPT_ENABLE == 0
}

fn matches(base: Base, hart: usize, madt: Madt<'_>) -> bool {
    base.major
        && madt.local_controller_address() == Some(base.physical.start())
        && x86_64::boot_identity() == hart
        && Destination::new(hart).is_some_and(|id| madt.enabled_local_apic(id.0))
}

pub(crate) fn matches_madt(hart: usize, madt: Madt<'_>) -> bool {
    base().is_some_and(|base| matches(base, hart, madt))
}

/// The actual exclusive local ICR owner, not a firmware description.
/// Drop neither retires a device mapping nor settles an uncertain send.
#[must_use]
pub struct LocalApic {
    base: Base,
    hart: usize,
    window: DeviceWindow,
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
    ) -> Result<Self, (DeviceWindow, Failure)> {
        let Some(base) = base() else {
            return Err((window, Failure::Scope));
        };
        if !masked()
            || local::try_current_hart().is_none_or(|current| current.get() != hart)
            || !matches(base, hart, madt)
            || window.physical() != base.physical
            || !window.is_aligned(Alignment::new(PAGE).expect("APIC page alignment"))
        {
            return Err((window, Failure::Scope));
        }
        // These are non-destructive read-only hardware observations.
        let id = unsafe { window.read_u32(ID) }.expect("APIC ID geometry");
        let version = unsafe { window.read_u32(VERSION) }.expect("APIC version geometry");
        if id >> super::DESTINATION_SHIFT != hart as u32 || !integrated(version) {
            return Err((window, Failure::Scope));
        }
        Ok(Self {
            base,
            hart,
            window,
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

    /// Check the original local authority and effect state before Boot
    /// prepares (or rewrites) its reserved trampoline.
    pub(in crate::metal) fn preflight(
        &self,
        hart: usize,
        page: PhysicalAddress,
    ) -> Result<(), Failure> {
        if !self.can_start(hart)
            || Vector::new(page).is_none()
            || local::try_current_hart().is_none_or(|current| current.get() != self.hart)
            || base() != Some(self.base)
            || !masked()
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
    pub(in crate::metal) unsafe fn start(
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
