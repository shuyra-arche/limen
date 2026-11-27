//! INIT/SIPI command encoding and original startup effects.

use limen_treaty::model::PhysicalAddress;

use super::{
    Failure, LocalApic,
    registers::{Destination, Icr, LEVEL_ASSERT, PAGE},
};

const DELIVERY_INIT: u32 = 5 << 8;
const DELIVERY_STARTUP: u32 = 6 << 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Vector(u8);

impl Vector {
    pub(super) fn new(page: PhysicalAddress) -> Option<Self> {
        let address = page.get();
        if address == 0 || !address.is_multiple_of(PAGE) {
            return None;
        }
        u8::try_from(address / PAGE).ok().map(Self)
    }
}

pub(super) enum Command {
    Init,
    Startup(Vector),
}

impl Command {
    /// Explicit physical destination and edge trigger; never echo ICR status.
    pub(super) fn bits(self) -> u32 {
        match self {
            Self::Init => LEVEL_ASSERT | DELIVERY_INIT,
            Self::Startup(vector) => LEVEL_ASSERT | DELIVERY_STARTUP | u32::from(vector.0),
        }
    }
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

impl<'map> LocalApic<'map> {
    /// Retained state of the separate INIT/SIPI startup sequence.
    pub fn launch_state(&self) -> State {
        match self.icr {
            Icr::Launching(effect) => State::Sending(effect),
            Icr::LaunchUnconfirmed(effect) => State::Unconfirmed(effect),
            Icr::Idle | Icr::Notifying(_) => State::Idle,
        }
    }

    /// Pure target geometry and original scope, not live startup authority.
    pub fn can_start(&self, hart: usize) -> bool {
        self.base.major && hart != self.hart && Destination::new(hart).is_some()
    }

    /// Borrow the exact controller for the requested target and page.
    /// Preparation neither writes ICR nor establishes target arrival.
    pub fn prepare(
        &mut self,
        hart: usize,
        page: PhysicalAddress,
    ) -> Result<Prepared<'_, 'map>, Failure> {
        self.preflight(hart, page)?;
        if !self.wait_icr() {
            return Err(Failure::Busy);
        }
        Ok(Prepared {
            controller: self,
            hart,
            page,
        })
    }

    fn preflight(&self, hart: usize, page: PhysicalAddress) -> Result<(), Failure> {
        if !self.can_start(hart)
            || Vector::new(page).is_none()
            || self.probe.base_for(self.hart) != Some(self.base)
        {
            return Err(Failure::Scope);
        }
        if self.icr != Icr::Idle {
            return Err(Failure::Busy);
        }
        Ok(())
    }

    fn send(&mut self, effect: Effect, destination: Destination, command: Command) {
        self.icr = Icr::Launching(effect);
        self.write_icr(destination, command.bits());
    }

    /// # Safety
    /// The caller owns the exact inactive target, published entry and its
    /// physical trampoline. All inputs remain valid until target arrival
    /// and all old accesses settle. The delay keeps the trusted Hart and
    /// masked state, never reenters ICR control, and reports a full interval.
    /// An uncertain sequence must retain this owner and its mapping.
    unsafe fn start(
        &mut self,
        hart: usize,
        page: PhysicalAddress,
        delay: fn(u32) -> bool,
    ) -> Result<(), Failure> {
        self.preflight(hart, page)?;
        let destination = Destination::new(hart).expect("checked destination");
        let vector = Vector::new(page).expect("checked vector");
        if !self.wait_icr() || !delay(1) {
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
            if !self.wait_icr() || !delay(interval) {
                self.icr = Icr::LaunchUnconfirmed(effect);
                return Err(Failure::Unconfirmed);
            }
        }
        // Only the local send sequence is settled; the AP's actual entry and
        // continued ownership of the trampoline remain with the launch caller.
        self.icr = Icr::Idle;
        Ok(())
    }
}

/// An exclusive, uncommitted startup preparation for one exact target and page.
/// Dropping it before sending releases the borrow without a hardware effect.
#[must_use]
pub struct Prepared<'a, 'map> {
    controller: &'a mut LocalApic<'map>,
    hart: usize,
    page: PhysicalAddress,
}

impl Prepared<'_, '_> {
    /// # Safety
    /// The original target is inactive and exclusively owned, and its
    /// prepared trampoline and entry have been published. Preserve their
    /// mappings and lifetimes until arrival and all old effects settle.
    /// The delay preserves the current Hart, masked state and exclusive
    /// ICR ownership and reports the full requested interval.
    pub unsafe fn start(self, delay: fn(u32) -> bool) -> Result<(), Failure> {
        unsafe { self.controller.start(self.hart, self.page, delay) }
    }
}
