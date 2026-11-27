//! The context retains the original claim through its one completion write.

use super::Route;
use crate::metal::{
    interrupt::internal::supervisor::{Cause, Reception},
    memory::mmu::internal::device::DeviceWindow,
};
use limen_treaty::model::Alignment;

/// Actual MMIO ordering. The caller additionally owns the interconnect's
/// readback and completion contract; a fence alone does not drain gateways.
#[derive(Clone, Copy)]
pub struct Barrier(fn());

#[cfg(all(limen_target, target_arch = "riscv64"))]
impl Barrier {
    pub const fn native() -> Self {
        Self(|| unsafe { core::arch::asm!("fence iorw, iorw", options(nostack)) })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Failure {
    Scope,
    Busy,
    Exhausted,
    Unconfirmed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum State {
    Prepared,
    Opening,
    Open,
    Receiving(u64),
    Received(u64, u32),
    Completing(u64, u32),
    Awaiting(u64, u32),
    Shutting,
    Shut,
}

/// One preallocated reception position. Dropping a claim borrow keeps the
/// original context, source and round available for later settlement.
#[must_use]
pub struct Claim<'b, 'a> {
    owner: &'b mut Context<'a>,
    round: u64,
    source: u32,
}

impl Claim<'_, '_> {
    pub fn round(&self) -> u64 {
        self.round
    }
    pub fn source(&self) -> u32 {
        self.source
    }

    /// # Safety
    /// The device owner has serviced this event and the original software
    /// notification has settled or has durable storage elsewhere.
    pub unsafe fn complete(&mut self) -> Result<(), Failure> {
        self.owner.context()?;
        if self.owner.state != State::Received(self.round, self.source)
            || self.owner.read(self.owner.route.enable()) & self.owner.route.bit() == 0
        {
            return Err(Failure::Busy);
        }
        self.owner.state = State::Completing(self.round, self.source);
        self.owner
            .write(self.owner.route.threshold() + 4, self.source);
        self.owner.state = State::Awaiting(self.round, self.source);
        Ok(())
    }

    /// # Safety
    /// Provide the platform's evidence that this exact completion write
    /// reached the original enabled context and was accepted by its gateway.
    /// Never infer this from a second destructive claim-register read.
    pub unsafe fn confirm(&mut self) -> Result<(), Failure> {
        self.owner.context()?;
        if self.owner.state != State::Awaiting(self.round, self.source) {
            return Err(Failure::Busy);
        }
        self.owner.state = State::Open;
        Ok(())
    }
}

#[must_use]
pub struct Context<'a> {
    route: Route<'a>,
    mapping: &'a mut DeviceWindow,
    local: &'a mut Reception,
    barrier: Barrier,
    priority: u32,
    threshold: u32,
    next: u64,
    state: State,
}

impl<'a> Context<'a> {
    /// # Safety
    /// The caller exclusively owns the original context, all its IE words,
    /// and this source's global priority and gateway. All other contexts
    /// exclude this source, prior claims and events have settled, and the
    /// stopped device and original mapping stay owned through final shutdown.
    /// Non-destructive register readbacks confirm the preceding ordered writes
    /// on this interconnect. Retain the owner and window on partial failure.
    pub unsafe fn admit(
        route: Route<'a>,
        mapping: &'a mut DeviceWindow,
        local: &'a mut Reception,
        barrier: Barrier,
    ) -> Result<Self, Failure> {
        if route.registers() != mapping.physical()
            || !mapping.is_aligned(Alignment::new(4096).unwrap())
            || local.hart() != route.hart
            || local.cause() != Cause::External
            || !local.valid()
        {
            return Err(Failure::Scope);
        }
        if local.enabled() {
            return Err(Failure::Busy);
        }
        let mut context = Self {
            route,
            mapping,
            local,
            barrier,
            priority: 0,
            threshold: 0,
            next: 1,
            state: State::Prepared,
        };
        for word in 0..=(route.sources as usize / 32) {
            if context.read(route.enables() + 4 * word) != 0 {
                return Err(Failure::Busy);
            }
        }
        context.priority = context.read(route.priority());
        context.threshold = context.read(route.threshold());
        Ok(context)
    }
    pub fn state(&self) -> State {
        self.state
    }
    fn context(&self) -> Result<(), Failure> {
        self.local.valid().then_some(()).ok_or(Failure::Scope)
    }
    fn read(&mut self, offset: usize) -> u32 {
        (self.barrier.0)();
        let v = unsafe { self.mapping.read_u32(offset) }.expect("admitted PLIC window");
        (self.barrier.0)();
        v
    }
    fn write(&mut self, offset: usize, value: u32) {
        (self.barrier.0)();
        unsafe { self.mapping.write_u32(offset, value) }.expect("admitted PLIC window");
        (self.barrier.0)();
    }
    fn program(&mut self, offset: usize, value: u32) -> Result<(), Failure> {
        self.write(offset, value);
        (self.read(offset) == value)
            .then_some(())
            .ok_or(Failure::Unconfirmed)
    }

    /// # Safety
    /// A trusted, capacity-bounded local reception path must exist before
    /// the caller unmasks global delivery or starts the device.
    pub unsafe fn open(&mut self) -> Result<(), Failure> {
        self.context()?;
        if self.state != State::Prepared {
            return Err(Failure::Busy);
        }
        self.state = State::Opening;
        self.program(self.route.priority(), 1)?;
        self.program(self.route.threshold(), 0)?;
        self.program(self.route.enable(), self.route.bit())?;
        self.local.set(true);
        if !self.local.enabled() {
            return Err(Failure::Unconfirmed);
        }
        self.state = State::Open;
        Ok(())
    }
    pub fn claim(&mut self) -> Result<Option<Claim<'_, 'a>>, Failure> {
        self.context()?;
        if self.state != State::Open {
            return Err(Failure::Busy);
        }
        let round = self.next;
        self.next = round.checked_add(1).ok_or(Failure::Exhausted)?;
        self.state = State::Receiving(round);
        let source = self.read(self.route.threshold() + 4);
        if source == 0 {
            self.state = State::Open;
            return Ok(None);
        }
        self.state = State::Received(round, source);
        if source != self.route.source {
            return Err(Failure::Unconfirmed);
        }
        Ok(self.current())
    }
    pub fn current(&mut self) -> Option<Claim<'_, 'a>> {
        match self.state {
            State::Received(round, source) | State::Awaiting(round, source)
                if source == self.route.source =>
            {
                Some(Claim {
                    owner: self,
                    round,
                    source,
                })
            }
            _ => None,
        }
    }
    /// # Safety
    /// The device and every old gateway request are drained and all future
    /// old assertions and in-flight writes excluded, including retriggers
    /// from the last completion. A zero pending bit is insufficient.
    pub unsafe fn shut(&mut self) -> Result<(), Failure> {
        self.context()?;
        if self.state != State::Open {
            return Err(Failure::Busy);
        }
        self.state = State::Shutting;
        self.local.set(false);
        if self.local.enabled() {
            return Err(Failure::Unconfirmed);
        }
        self.program(self.route.enable(), 0)?;
        self.program(self.route.threshold(), self.threshold)?;
        self.program(self.route.priority(), self.priority)?;
        self.state = State::Shut;
        Ok(())
    }
}
