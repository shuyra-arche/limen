//! Non-secure stage-2, 4 KiB granule, three levels, 32-bit IOVA and output.

use super::{
    ABORT, CR0, CR0_ACK, DeviceWindow, Error, GBPA, IDR0, IDR5, SMMU_ENABLE, UPDATE, read,
};
use limen_treaty::model::PhysicalAddress;

pub struct Format(());
#[derive(Clone, Copy, Debug)]
pub enum Access {
    Read,
    Write,
    ReadWrite,
}

impl Format {
    /// # Safety
    /// Read the actual SMMUv3 reported by the platform, under its trusted
    /// MMIO and ownership contract. Accept only the AArch64 table format.
    pub unsafe fn probe(window: &mut DeviceWindow) -> Result<Self, Error> {
        let idr0 = unsafe { read(window, IDR0)? };
        let idr5 = unsafe { read(window, IDR5)? };
        if idr0 & 1 == 0 || (idr0 >> 2) & 2 == 0 || idr5 & (1 << 4) == 0 {
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
    pub fn stream(&self, root: PhysicalAddress, vmid: u8) -> Result<[u64; 8], Error> {
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

#[must_use]
pub struct Installation<'a> {
    window: &'a mut DeviceWindow,
    order: fn(),
}
impl Installation<'_> {
    pub fn observe(&mut self) -> Result<(), Error> {
        (self.order)();
        if unsafe { read(self.window, CR0_ACK)? } != SMMU_ENABLE {
            return Err(Error::Busy);
        }
        Ok(())
    }
}

/// Install one linear table from a disabled interface with acknowledged
/// GBPA.ABORT. The acknowledgement establishes selection, not retirement.
///
/// # Safety
/// Own all affected streams and retain source isolation until the required
/// configuration and translation invalidations complete. Keep the entire
/// aligned table (2^bits entries of 64 bytes) and reachable storage live in
/// coherent device-accessible memory, including on partial publication, loss
/// or reset. All alias routes and absent streams must already be accounted
/// for. `order` publishes the backing memory and orders device I/O.
pub unsafe fn install(
    window: &mut DeviceWindow,
    base: PhysicalAddress,
    bits: u8,
    order: fn(),
) -> Result<Installation<'_>, Error> {
    const STREAM_TABLE: usize = 0x80;
    const STREAM_CONFIG: usize = 0x88;
    let idr1 = unsafe { read(window, 4)? };
    if idr1 & (1 << 30) != 0 || u32::from(bits) > (idr1 & 63) || bits > 31 {
        return Err(Error::Rejected);
    }
    let address = base.get();
    if !address.is_multiple_of(64u64 << bits) || address >> 52 != 0 {
        return Err(Error::Window);
    }
    if unsafe { read(window, CR0)? | read(window, CR0_ACK)? } != 0 {
        return Err(Error::Active);
    }
    if unsafe { read(window, GBPA)? } & (ABORT | UPDATE) != ABORT {
        return Err(Error::Rejected);
    }
    unsafe { window.write_u64(STREAM_TABLE, address.to_le()) }.map_err(|_| Error::Window)?;
    unsafe { window.write_u32(STREAM_CONFIG, u32::from(bits).to_le()) }
        .map_err(|_| Error::Window)?;
    order();
    if unsafe { read(window, STREAM_CONFIG)? } & 0x3ffff != u32::from(bits)
        || u64::from_le(unsafe { window.read_u64(STREAM_TABLE) }.map_err(|_| Error::Window)?)
            & ((1 << 52) - 1)
            != address
    {
        return Err(Error::Rejected);
    }
    unsafe { window.write_u32(CR0, SMMU_ENABLE.to_le()) }.map_err(|_| Error::Window)?;
    Ok(Installation { window, order })
}
