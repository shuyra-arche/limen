//! Non-nested fixed-vector reception on the owning xAPIC processor.

use super::{Failure, LocalApic};
use crate::metal::interrupt::treaty::{ClaimConfirmation, ClaimId, Completion, HardwareClaim};

const ISR: usize = 0x100;
const IRR: usize = 0x200;
const EOI: usize = 0xb0;
const SPURIOUS: usize = 0xf0;
const SOFTWARE_ENABLE: u32 = 1 << 8;
const SUPPRESS_BROADCAST: u32 = 1 << 12;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Reception {
    Idle,
    Received(u8),
    /// The EOI write escaped. Never repeat it on this evidence.
    Unconfirmed(u8),
    /// A vector entered hardware after the last representable claim round.
    Exhausted(u8),
}

impl Reception {
    /// Record a vector already accepted by this processor's hardware.
    fn record(&mut self, round: &mut u64, vector: u8) -> Result<(), Failure> {
        let Some(next) = round.checked_add(1) else {
            *self = Self::Exhausted(vector);
            return Err(Failure::Busy);
        };
        *round = next;
        *self = Self::Received(vector);
        Ok(())
    }

    fn involves(self, vector: u8) -> bool {
        matches!(self, Self::Received(old) | Self::Unconfirmed(old) | Self::Exhausted(old) if old == vector)
    }
}

/// A fixed vector reserved on one original Local APIC.
#[derive(Debug)]
#[must_use]
pub struct VectorLease {
    processor: u8,
    vector: u8,
}

impl VectorLease {
    pub fn vector(&self) -> u8 {
        self.vector
    }

    pub(crate) fn matches(&self, processor: u8, vector: u8) -> bool {
        self.processor == processor && self.vector == vector
    }
}

/// Exclusive borrow of the non-nested reception slot. No later vector can
/// be admitted while this claim can still complete an EOI.
#[must_use]
pub struct Claim<'a, 'map> {
    owner: &'a mut LocalApic<'map>,
    vector: u8,
}

impl Claim<'_, '_> {
    pub fn vector(&self) -> u8 {
        self.vector
    }

    /// # Safety
    /// The original software delivery is settled or durably retained. The
    /// device has been cleared or masked and the receiving processor remains
    /// exclusive with ordinary interrupts masked.
    pub unsafe fn complete(&mut self) -> Result<(), Failure> {
        self.owner.complete(self.vector)
    }

    /// Check whether this claim's original EOI cleared its ISR bit.
    pub fn confirm(&mut self) -> Result<(), Failure> {
        self.owner.confirm_reception(self.vector)
    }
}

impl<'map> LocalApic<'map> {
    fn vector_bit(vector: u8) -> (usize, u64) {
        (usize::from(vector) / 64, 1 << (vector % 64))
    }

    /// Reserve a native-quiet destination vector before publishing a physical
    /// source route. The owner must also keep unrelated sources from targeting it.
    pub fn reserve_vector(&mut self, vector: u8) -> Result<VectorLease, Failure> {
        self.reception_context()?;
        let processor = u8::try_from(self.processor).map_err(|_| Failure::Scope)?;
        let svr = unsafe { self.window.read_u32(SPURIOUS) }.expect("xAPIC SVR");
        if !Self::fixed_vector(vector, svr) {
            return Err(Failure::Scope);
        }
        let (bank, bit) = Self::vector_bit(vector);
        if self.vectors[bank] & bit != 0
            || self.reception_round == u64::MAX
            || self.vector_busy(vector)
        {
            return Err(Failure::Busy);
        }
        self.vectors[bank] |= bit;
        Ok(VectorLease { processor, vector })
    }

    /// Return a lease after the original source route has retired and its
    /// queued, accepted and recorded reception has drained. The source owner
    /// must separately stop further arrivals before returning the lease.
    pub fn release_vector(&mut self, lease: VectorLease) -> Result<(), (VectorLease, Failure)> {
        if usize::from(lease.processor) != self.processor || self.reception_context().is_err() {
            return Err((lease, Failure::Scope));
        }
        let (bank, bit) = Self::vector_bit(lease.vector);
        if self.vectors[bank] & bit == 0 || self.vector_busy(lease.vector) {
            return Err((lease, Failure::Busy));
        }
        self.vectors[bank] &= !bit;
        Ok(())
    }

    /// Reborrow the original reception without a new vector entry.
    pub fn current(&mut self) -> Option<Claim<'_, 'map>> {
        let (Reception::Received(vector) | Reception::Unconfirmed(vector)) = self.reception else {
            return None;
        };
        Some(Claim {
            owner: self,
            vector,
        })
    }

    pub fn reception(&self) -> Reception {
        self.reception
    }

    pub(super) fn reception_context(&self) -> Result<(), Failure> {
        if self.probe.base_for(self.processor) != Some(self.base) {
            return Err(Failure::Scope);
        }
        Ok(())
    }

    fn top(&mut self) -> Option<u8> {
        // The caller retains this processor with ordinary interrupts masked.
        for bank in (0..8).rev() {
            let word = unsafe { self.window.read_u32(ISR + 16 * bank) }.expect("xAPIC ISR bank");
            if word != 0 {
                return Some((bank as u8) * 32 + (31 - word.leading_zeros() as u8));
            }
        }
        None
    }

    /// # Safety
    /// Called on the destination of an actual fixed delivery recorded at
    /// vector entry. Retain the original binding and reserved reception
    /// slot until its software and hardware obligations have been settled.
    /// No other context manipulates the ISR, EOI or spurious-vector register.
    pub unsafe fn receive(&mut self, vector: u8) -> Result<Claim<'_, 'map>, Failure> {
        self.reception_context()?;
        if self.reception != Reception::Idle {
            return Err(Failure::Busy);
        }
        let svr = unsafe { self.window.read_u32(SPURIOUS) }.expect("xAPIC SVR");
        let (bank, bit) = Self::vector_bit(vector);
        if !Self::fixed_vector(vector, svr)
            || self.vectors[bank] & bit == 0
            || self.top() != Some(vector)
        {
            return Err(Failure::Scope);
        }
        self.reception.record(&mut self.reception_round, vector)?;
        Ok(Claim {
            owner: self,
            vector,
        })
    }

    /// # Safety
    /// The original received route is durably recorded and the source has
    /// been cleared or masked to permit the corresponding EOI. Ordinary
    /// interrupts stay masked; no competing EOI can pop another ISR bit.
    fn complete(&mut self, vector: u8) -> Result<(), Failure> {
        self.reception_context()?;
        if self.reception != Reception::Received(vector) || self.top() != Some(vector) {
            return Err(Failure::Busy);
        }
        self.reception = Reception::Unconfirmed(vector);
        unsafe { self.window.write_u32(EOI, 0) }.expect("xAPIC EOI");
        self.confirm_reception(vector)
    }

    /// Observe the same outstanding EOI without issuing another write.
    /// Retain the original route until the original ISR bit clears.
    fn confirm_reception(&mut self, vector: u8) -> Result<(), Failure> {
        self.reception_context()?;
        if self.reception != Reception::Unconfirmed(vector) {
            return Err(Failure::Busy);
        }
        if self.in_service(vector) {
            return Err(Failure::Unconfirmed);
        }
        self.reception = Reception::Idle;
        Ok(())
    }

    fn fixed_vector(vector: u8, svr: u32) -> bool {
        vector >= 32
            && vector != svr as u8
            && svr & SOFTWARE_ENABLE != 0
            && svr & SUPPRESS_BROADCAST == 0
    }

    fn in_service(&mut self, vector: u8) -> bool {
        self.vector_set(ISR, vector)
    }

    fn vector_set(&mut self, register: usize, vector: u8) -> bool {
        let bank = usize::from(vector) / 32;
        let bit = 1 << (vector % 32);
        let word =
            unsafe { self.window.read_u32(register + 16 * bank) }.expect("xAPIC vector bank");
        word & bit != 0
    }

    pub(super) fn vector_busy(&mut self, vector: u8) -> bool {
        self.reception.involves(vector) || self.vector_set(IRR, vector) || self.in_service(vector)
    }
}

impl crate::metal::interrupt::treaty::claim::Sealed for Claim<'_, '_> {}

impl HardwareClaim for Claim<'_, '_> {
    type Error = Failure;

    fn identity(&self) -> ClaimId {
        ClaimId::new(
            self.owner.base.physical.start(),
            self.owner.processor as u64,
            self.owner.reception_round,
            u32::from(self.vector()),
        )
    }

    fn confirm(&mut self, evidence: ClaimConfirmation) -> Result<Completion, Failure> {
        if evidence.id() != self.identity() || evidence.accepted() {
            return Err(Failure::Scope);
        }
        match Claim::confirm(self) {
            Ok(()) => Ok(Completion::Settled),
            Err(Failure::Unconfirmed) => Ok(Completion::AwaitingObservation),
            Err(error) => Err(error),
        }
    }

    unsafe fn finish(&mut self) -> Result<Completion, Failure> {
        match unsafe { self.complete() } {
            Ok(()) => Ok(Completion::Settled),
            Err(Failure::Unconfirmed) => Ok(Completion::AwaitingObservation),
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
#[path = "../../../../../../tests/metal/interrupt/x86_64/internal/apic/reception.rs"]
mod tests;
