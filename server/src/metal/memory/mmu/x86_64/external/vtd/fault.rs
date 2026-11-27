//! Original primary fault observation and acknowledgement.

use super::{Controller, Error, FAULT_OVERFLOW, FAULT_PENDING, FAULT_STATUS, FAULT_VALID};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Fault {
    slot: usize,
    address: u64,
    detail: u64,
}

impl Fault {
    pub fn address(self) -> u64 {
        self.address & !0xfff
    }

    pub fn requester(self) -> u16 {
        self.detail as u16
    }

    pub fn reason(self) -> u8 {
        (self.detail >> 32) as u8
    }

    pub fn write(self) -> bool {
        self.detail & (1 << 62) == 0
    }
}

impl Controller<'_> {
    /// Observe the first valid primary fault without clearing it.
    pub fn primary_fault(&mut self) -> Result<Option<Fault>, Error> {
        let status =
            unsafe { self.window.read_u32(FAULT_STATUS) }.expect("validated VT-d fault status");
        if status & FAULT_OVERFLOW != 0 {
            return Err(Error::Unconfirmed);
        }
        if status & FAULT_PENDING == 0 {
            return Ok(None);
        }
        let slot = ((status >> 8) & 0xff) as usize;
        if slot >= self.fault_count {
            return Err(Error::Unconfirmed);
        }
        let offset = self.fault_base + 16 * slot;
        let detail = self.read(offset + 8);
        let address = self.read(offset);
        if detail & FAULT_VALID == 0 || self.read(offset + 8) != detail {
            return Err(Error::Pending);
        }
        Ok(Some(Fault {
            slot,
            address,
            detail,
        }))
    }

    /// # Safety
    /// This is the original fault observed under this same controller.
    /// Quiesce its sources and exclude independent record acknowledgement
    /// until this exact record is cleared and the acknowledgement confirmed.
    pub unsafe fn acknowledge_fault(&mut self, expected: Fault) -> Result<(), Error> {
        if self.primary_fault()? != Some(expected) {
            return Err(Error::Stale);
        }
        let offset = self.fault_base + 16 * expected.slot + 8;
        self.write(offset, FAULT_VALID);
        (self.read(offset) & FAULT_VALID == 0)
            .then_some(())
            .ok_or(Error::Unconfirmed)
    }
}
