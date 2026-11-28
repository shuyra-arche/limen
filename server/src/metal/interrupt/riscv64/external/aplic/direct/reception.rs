//! A destructive read and its original claim belong to one IDC.

use super::{
    super::{Domain, Effect, Failure, Mode, Source, State},
    Context,
    encoding::{Interrupt, Target},
};
use crate::metal::interrupt::treaty::{
    ClaimConfirmation, ClaimId, Completion, HardwareClaim, Receive, claim::Sealed,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Record {
    round: u64,
    source: u32,
    priority: u8,
    raw: u32,
}

enum Slot {
    Empty,
    Reading(u64),
    Masking(Record),
    Received(Record),
    Settling(Record),
    Uncertain(u64, u32),
}

pub(super) struct Reception {
    slot: Slot,
    next: u64,
}

impl Reception {
    pub(super) const fn new() -> Self {
        Self {
            slot: Slot::Empty,
            next: 1,
        }
    }

    pub(super) fn quiet(&self) -> Result<(), Failure> {
        if !matches!(self.slot, Slot::Empty) {
            return Err(Failure::Busy);
        }
        Ok(())
    }

    pub(super) fn has_round(&self) -> Result<(), Failure> {
        self.next
            .checked_add(1)
            .map(|_| ())
            .ok_or(Failure::Exhausted)
    }

    fn begin(&mut self) -> Result<(), Failure> {
        self.quiet()?;
        let round = self.next;
        self.next = round.checked_add(1).ok_or(Failure::Exhausted)?;
        self.slot = Slot::Reading(round);
        Ok(())
    }

    fn observe(
        &mut self,
        raw: u32,
        source: impl FnOnce(u32) -> Result<Source, Failure>,
    ) -> Result<Option<(Record, Mode)>, Failure> {
        let Slot::Reading(round) = self.slot else {
            return Err(Failure::Busy);
        };
        match Interrupt::read(raw) {
            Some(Interrupt::Empty) => {
                self.slot = Slot::Empty;
                return Ok(None);
            }
            Some(Interrupt::Claimed {
                source: id,
                priority,
            }) if priority == Target::PRIORITY => {
                if let Ok(Source::Enabled(mode)) = source(id) {
                    let record = Record {
                        round,
                        source: id,
                        priority,
                        raw,
                    };
                    self.slot = Slot::Masking(record);
                    return Ok(Some((record, mode)));
                }
            }
            _ => {}
        }
        self.slot = Slot::Uncertain(round, raw);
        Err(Failure::Unconfirmed)
    }

    fn confirm_mask(&mut self, record: Record) -> Result<(), Failure> {
        if !matches!(self.slot, Slot::Masking(pending) if pending == record) {
            return Err(Failure::Busy);
        }
        self.slot = Slot::Received(record);
        Ok(())
    }

    fn current(&self) -> Option<Record> {
        match self.slot {
            Slot::Received(record) => Some(record),
            _ => None,
        }
    }

    pub(super) fn uncertain(&self) -> Option<(u64, Option<u32>)> {
        match self.slot {
            Slot::Reading(round) => Some((round, None)),
            Slot::Masking(record) | Slot::Settling(record) => {
                Some((record.round, Some(record.raw)))
            }
            Slot::Uncertain(round, raw) => Some((round, Some(raw))),
            _ => None,
        }
    }

    fn settle(&mut self, record: Record) -> Result<(), Failure> {
        if self.current() != Some(record) {
            return Err(Failure::Busy);
        }
        self.slot = Slot::Empty;
        Ok(())
    }
}

/// One scoped use of the corresponding Domain and IDC. The IDC keeps its own
/// original reception record after the borrow ends.
#[must_use]
pub struct Receiver<'access, 'description, 'resources> {
    domain: &'access mut Domain<'description, 'resources>,
    context: &'access mut Context<'resources>,
}

impl<'access, 'description, 'resources> Receiver<'access, 'description, 'resources> {
    pub(in super::super) fn new(
        domain: &'access mut Domain<'description, 'resources>,
        context: &'access mut Context<'resources>,
    ) -> Self {
        Self { domain, context }
    }

    fn borrow_claim(&mut self, record: Record) -> Claim<'_, 'description, 'resources> {
        Claim {
            receiver: Receiver::new(self.domain, self.context),
            record,
        }
    }

    /// Reserve this IDC's only slot before performing its destructive read.
    pub fn claim(&mut self) -> Result<Option<Claim<'_, 'description, 'resources>>, Failure> {
        self.context.check()?;
        if self.domain.state() != State::Open {
            return Err(Failure::Busy);
        }
        self.context.reception.begin()?;
        let raw = self.context.claim_word();
        let Some((record, mode)) = self
            .context
            .reception
            .observe(raw, |id| self.domain.source(id))
            .map_err(|_| self.domain.unconfirmed(self.context, Effect::Receive))?
        else {
            return Ok(None);
        };
        self.domain.mask(self.context, record.source, mode)?;
        self.context.reception.confirm_mask(record)?;
        Ok(Some(self.borrow_claim(record)))
    }

    /// Reborrow this IDC's confirmed claim without another read.
    pub fn current(&mut self) -> Option<Claim<'_, 'description, 'resources>> {
        let record = self.context.reception.current()?;
        if self.domain.state() != State::Open
            || !matches!(self.domain.source(record.source), Ok(Source::Masked(_)))
        {
            return None;
        }
        Some(self.borrow_claim(record))
    }

    fn settle(&mut self, record: Record) -> Result<(), Failure> {
        self.context.check()?;
        if self.domain.state() != State::Open || self.context.reception.current() != Some(record) {
            return Err(Failure::Busy);
        }
        if let Err(error) = self.domain.settle_source(self.context, record.source) {
            if error == Failure::Unconfirmed {
                self.context.reception.slot = Slot::Settling(record);
            }
            return Err(error);
        }
        self.context.reception.settle(record)
    }

    fn confirm_settlement(&mut self, record: Record) -> Result<(), Failure> {
        self.context.check()?;
        if !matches!(self.context.reception.slot, Slot::Settling(original) if original == record) {
            return Err(Failure::Busy);
        }
        if self.domain.state() != State::Open {
            self.domain.confirm_mask(self.context, record.source)?;
        }
        self.domain.settle_source(self.context, record.source)?;
        self.context.reception.slot = Slot::Empty;
        Ok(())
    }
}

/// The claim belongs to the borrowing IDC and the exact recorded round.
#[must_use]
pub struct Claim<'access, 'description, 'resources> {
    receiver: Receiver<'access, 'description, 'resources>,
    record: Record,
}

impl Claim<'_, '_, '_> {
    pub fn source(&self) -> u32 {
        self.record.source
    }

    pub fn priority(&self) -> u8 {
        self.record.priority
    }

    pub fn round(&self) -> u64 {
        self.record.round
    }

    /// # Safety
    /// Settle or durably hand off this exact software obligation. Clear the
    /// original level cause in the device before settling its claim.
    pub unsafe fn settle(&mut self) -> Result<(), Failure> {
        self.receiver.settle(self.record)
    }
}

impl Sealed for Claim<'_, '_, '_> {}
impl HardwareClaim for Claim<'_, '_, '_> {
    type Error = Failure;

    fn identity(&self) -> ClaimId {
        ClaimId::new(
            self.receiver.context.domain().start(),
            self.receiver.context.destination().index() as u64,
            self.round(),
            self.source(),
        )
    }

    fn confirm(&mut self, evidence: ClaimConfirmation) -> Result<Completion, Failure> {
        if evidence.id() != self.identity() || evidence.accepted() {
            return Err(Failure::Scope);
        }
        match self.receiver.confirm_settlement(self.record) {
            Ok(()) => Ok(Completion::Settled),
            Err(Failure::Unconfirmed) => Ok(Completion::AwaitingObservation),
            Err(error) => Err(error),
        }
    }

    unsafe fn finish(&mut self) -> Result<Completion, Failure> {
        match unsafe { self.settle() } {
            Ok(()) => Ok(Completion::Settled),
            Err(Failure::Unconfirmed) => Ok(Completion::AwaitingObservation),
            Err(error) => Err(error),
        }
    }
}

impl Sealed for Receiver<'_, '_, '_> {}
impl<'description, 'resources> Receive for Receiver<'_, 'description, 'resources> {
    type Error = Failure;
    type Claim<'receiver>
        = Claim<'receiver, 'description, 'resources>
    where
        Self: 'receiver;

    fn receive(&mut self) -> Result<Option<Self::Claim<'_>>, Failure> {
        self.claim()
    }

    fn pending_claim(&mut self) -> Result<Option<Self::Claim<'_>>, Failure> {
        self.context.check()?;
        match self.context.reception.slot {
            Slot::Masking(record) => {
                self.domain.confirm_mask(self.context, record.source)?;
                self.context.reception.confirm_mask(record)?;
            }
            Slot::Settling(record) => return Ok(Some(self.borrow_claim(record))),
            Slot::Reading(_) | Slot::Uncertain(_, _) => return Err(Failure::Unconfirmed),
            Slot::Empty | Slot::Received(_) => {}
        }
        Ok(self.current())
    }
}

#[cfg(test)]
#[path = "../../../../../../../tests/metal/interrupt/riscv64/external/aplic/direct/reception.rs"]
mod tests;
