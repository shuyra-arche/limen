//! The original IDC, its registers, and its local supervisor reception line.

use limen_treaty::model::PhysicalRange;

use super::super::{Failure, registers::Words};
use super::reception::Reception;
use crate::metal::interrupt::riscv64::internal::supervisor::{Cause, Reception as LocalReception};

const DELIVERY: usize = 0;
const FORCE: usize = 4;
const THRESHOLD: usize = 8;
const CLAIM: usize = 28;

/// Firmware's dense IDC index and its original hardware Hart.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Destination {
    hart: u64,
    index: usize,
    contexts: usize,
}

impl Destination {
    pub(in super::super) const fn new(hart: u64, index: usize, contexts: usize) -> Self {
        Self {
            hart,
            index,
            contexts,
        }
    }

    pub const fn hart(self) -> u64 {
        self.hart
    }

    pub const fn index(self) -> usize {
        self.index
    }

    pub const fn contexts(self) -> usize {
        self.contexts
    }

    /// The present backend admits only the actual single target.
    pub(in super::super) fn verify_single(self) -> Result<(), Failure> {
        if self.contexts != 1 || self.index != 0 {
            return Err(Failure::Scope);
        }
        Ok(())
    }
}

/// One physical IDC and the unique software record of its destructive reads.
#[must_use]
pub struct Context<'a> {
    domain: PhysicalRange,
    destination: Destination,
    registers: Words<'a>,
    local: &'a mut LocalReception,
    pub(super) reception: Reception,
}

impl<'a> Context<'a> {
    pub(in super::super) fn admit(
        domain: PhysicalRange,
        destination: Destination,
        registers: Words<'a>,
        local: &'a mut LocalReception,
    ) -> Result<Self, Failure> {
        if local.hart() != destination.hart() || local.cause() != Cause::External {
            return Err(Failure::Scope);
        }
        let instance = Self {
            domain,
            destination,
            registers,
            local,
            reception: Reception::new(),
        };
        instance.check()?;
        Ok(instance)
    }

    pub const fn destination(&self) -> Destination {
        self.destination
    }

    /// Leave uncertain results owned by their original slot.
    pub fn uncertain_claim(&self) -> Option<(u64, Option<u32>)> {
        self.reception.uncertain()
    }

    pub(in super::super) fn domain(&self) -> PhysicalRange {
        self.domain
    }

    pub(in super::super) fn check(&self) -> Result<(), Failure> {
        if !self.local.valid() {
            return Err(Failure::Context);
        }
        Ok(())
    }

    pub(in super::super) fn quiet(&self) -> Result<(), Failure> {
        self.reception.quiet()
    }

    pub(in super::super) fn has_round(&self) -> Result<(), Failure> {
        self.reception.has_round()
    }

    pub(in super::super) fn pristine(&mut self) -> bool {
        self.delivery(false) && self.registers.read(FORCE) == 0 && !self.enabled()
    }

    pub(in super::super) fn open(&mut self) {
        self.registers.write(THRESHOLD, 0);
        self.registers.write(DELIVERY, 1);
    }

    pub(in super::super) fn shut(&mut self) {
        self.registers.write(DELIVERY, 0);
    }

    pub(in super::super) fn delivery(&mut self, enabled: bool) -> bool {
        self.registers.read(DELIVERY) == u32::from(enabled)
    }

    pub(in super::super) fn threshold(&mut self) -> u32 {
        self.registers.read(THRESHOLD)
    }

    pub(in super::super) fn enabled(&self) -> bool {
        self.local.enabled()
    }

    pub(in super::super) fn set_enabled(&mut self, enabled: bool) {
        self.local.set(enabled);
    }

    pub(super) fn claim_word(&mut self) -> u32 {
        self.registers.read(CLAIM)
    }
}

#[cfg(test)]
#[path = "../../../../../../../tests/metal/interrupt/riscv64/external/aplic/direct/context.rs"]
mod tests;
