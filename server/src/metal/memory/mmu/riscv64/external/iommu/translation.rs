//! Single-stage Sv39 for unprivileged device requests with no process ID.

use super::{Error, PhysicalAddress, Profile};
use crate::metal::memory::mmu::riscv64::sv::{
    ACCESSED, DIRTY, PAGE_BYTES, PageNumber, READ, USER, WRITE,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Read,
    ReadWrite,
}

impl Profile {
    fn page(&self, address: PhysicalAddress) -> Result<PageNumber, Error> {
        let bytes = address.get();
        if !self.sv39 || !self.little_endian_tables {
            return Err(Error::Mode);
        }
        if bytes >> self.address_bits.min(56) != 0 {
            return Err(Error::Address);
        }
        PageNumber::new(address, PAGE_BYTES).ok_or(Error::Address)
    }

    pub(super) fn first_stage(&self, root: PhysicalAddress) -> Result<u64, Error> {
        const SV39: u64 = 8 << 60;
        Ok(SV39 | self.page(root)?.get())
    }

    pub fn branch(&self, child: PhysicalAddress) -> Result<u64, Error> {
        Ok(self.page(child)?.branch())
    }

    /// 4 KiB leaf, unprivileged data access, eager A/D, no executable or
    /// global translation. Sv39 cannot encode write-only access.
    pub fn leaf(&self, frame: PhysicalAddress, access: Access) -> Result<u64, Error> {
        let readable = READ | USER | ACCESSED;
        let writable_dirty = match access {
            Access::Read => 0,
            Access::ReadWrite => DIRTY | WRITE,
        };
        Ok(self.page(frame)?.leaf(readable | writable_dirty))
    }
}
