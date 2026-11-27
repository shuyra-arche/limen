//! Non-nested fixed-vector reception on the owning xAPIC Hart.

use super::{Failure, LocalApic};

const ISR: usize = 0x100;
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
}

/// Exclusive borrow of the non-nested reception slot. No later vector can
/// be admitted while this claim can still complete an EOI.
#[must_use]
pub struct Claim<'a> {
    owner: &'a mut LocalApic,
    vector: u8,
}

impl Claim<'_> {
    pub fn vector(&self) -> u8 {
        self.vector
    }

    /// # Safety
    /// The original software delivery is settled or durably retained. The
    /// device has been cleared or masked and the receiving Hart remains
    /// exclusive with ordinary interrupts masked.
    pub unsafe fn complete(&mut self) -> Result<(), Failure> {
        self.owner.complete(self.vector)
    }
}

impl LocalApic {
    /// Reborrow the original reception without a new vector entry.
    pub fn current(&mut self) -> Option<Claim<'_>> {
        let Reception::Received(vector) = self.reception else {
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

    fn reception_context(&self) -> Result<(), Failure> {
        if self.probe.base_for(self.hart) != Some(self.base) {
            return Err(Failure::Scope);
        }
        Ok(())
    }

    fn top(&mut self) -> Option<u8> {
        // The caller retains this Hart with ordinary interrupts masked.
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
    pub unsafe fn receive(&mut self, vector: u8) -> Result<Claim<'_>, Failure> {
        self.reception_context()?;
        if self.reception != Reception::Idle {
            return Err(Failure::Busy);
        }
        let svr = unsafe { self.window.read_u32(SPURIOUS) }.expect("xAPIC SVR");
        if vector < 32
            || vector == svr as u8
            || svr & SOFTWARE_ENABLE == 0
            || svr & SUPPRESS_BROADCAST != 0
            || self.top() != Some(vector)
        {
            return Err(Failure::Scope);
        }
        self.reception = Reception::Received(vector);
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
        if self.top() == Some(vector) {
            return Err(Failure::Unconfirmed);
        }
        self.reception = Reception::Idle;
        Ok(())
    }
}
