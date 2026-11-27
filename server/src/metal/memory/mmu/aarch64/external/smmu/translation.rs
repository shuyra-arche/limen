//! Non-secure stage-2, 4 KiB granule, three levels, 32-bit IOVA and output.

use super::{Error, Profile};
use limen_treaty::model::PhysicalAddress;

#[derive(Clone, Copy)]
pub struct Format(());
#[derive(Clone, Copy, Debug)]
pub enum Access {
    Read,
    Write,
    ReadWrite,
}

impl Format {
    pub(super) fn new(profile: Profile) -> Result<Self, Error> {
        if !profile.stage_two || !profile.aarch64_tables || !profile.granule_4k {
            return Err(Error::Rejected);
        }
        Ok(Self(()))
    }

    fn page(address: PhysicalAddress) -> Result<u64, Error> {
        let bytes = address.get();
        if !bytes.is_multiple_of(4096) || bytes >> 32 != 0 {
            return Err(Error::Window);
        }
        Ok(bytes)
    }
    /// Valid, translating STE. Untranslated traffic only, no ATS, PASID or
    /// stall. The caller publishes the complete table while sources are stopped.
    pub(super) fn stream(&self, root: PhysicalAddress, vmid: u8) -> Result<[u64; 8], Error> {
        // T0SZ=32, SL0=1 (start at L1), inner/outer WBWA, inner-shareable,
        // 4 KiB and 32-bit PA. Little-endian AArch64 walk with no HTTU.
        let vtcr: u64 = 32 | (1 << 6) | (1 << 8) | (1 << 10) | (3 << 12);
        Ok([
            1 | (6 << 1),
            0,
            u64::from(vmid) | (vtcr << 32) | (1 << 51),
            Self::page(root)?,
            0,
            0,
            0,
            0,
        ])
    }
    pub fn branch(&self, child: PhysicalAddress) -> Result<u64, Error> {
        Ok(Self::page(child)? | 3)
    }
    /// Last-level normal WB, inner-shareable, accessed, non-executable page.
    pub fn leaf(&self, frame: PhysicalAddress, access: Access) -> Result<u64, Error> {
        let rights = match access {
            Access::Read => 1,
            Access::Write => 2,
            Access::ReadWrite => 3,
        };
        Ok(Self::page(frame)? | (1 << 54) | (1 << 10) | (3 << 8) | (rights << 6) | (15 << 2) | 3)
    }
}
