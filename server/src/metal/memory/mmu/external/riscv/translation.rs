//! Single-stage Sv39 for unprivileged device requests with no process ID.

use super::{Error, PhysicalAddress, Profile};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Read,
    ReadWrite,
}

impl Profile {
    /// Locate a device context within a single-level 4 KiB directory.
    pub fn context_words(&self, device: u32) -> Result<core::ops::Range<usize>, Error> {
        let words = if self.extended_context { 8 } else { 4 };
        let index = device as usize;
        if index >= 512 / words {
            return Err(Error::Address);
        }
        Ok(index * words..(index + 1) * words)
    }

    fn page(&self, address: PhysicalAddress) -> Result<u64, Error> {
        let bytes = address.get();
        if !self.sv39 || !self.little_endian_tables {
            return Err(Error::Mode);
        }
        if !bytes.is_multiple_of(4096) || bytes >> self.address_bits.min(56) != 0 {
            return Err(Error::Address);
        }
        Ok(bytes >> 12)
    }

    /// The four common words of a device context. Use the hardware-selected
    /// DDT stride, zero all optional MSI fields, and publish the valid word
    /// last (or prepare the entire directory before installing its root).
    pub fn context(&self, root: PhysicalAddress, pscid: u32) -> Result<[u64; 4], Error> {
        if pscid >= 1 << 20 {
            return Err(Error::Address);
        }
        Ok([1, 0, u64::from(pscid) << 12, (8 << 60) | self.page(root)?])
    }

    pub fn branch(&self, child: PhysicalAddress) -> Result<u64, Error> {
        Ok((self.page(child)? << 10) | 1)
    }

    /// 4 KiB leaf, unprivileged data access, eager A/D, no executable or
    /// global translation. Sv39 cannot encode write-only access.
    pub fn leaf(&self, frame: PhysicalAddress, access: Access) -> Result<u64, Error> {
        let valid_read_user_accessed = (1 << 6) | (1 << 4) | (1 << 1) | 1;
        let writable_dirty = match access {
            Access::Read => 0,
            Access::ReadWrite => (1 << 7) | (1 << 2),
        };
        Ok((self.page(frame)? << 10) | valid_read_user_accessed | writable_dirty)
    }
}
