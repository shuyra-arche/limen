//! RISC-V IOMMU 1.0 device-directory control (DDTP).

use crate::metal::{
    memory::mmu::internal::device::DeviceWindow,
    platform::description::devicetree::{Error as TreeError, Node},
};
use limen_treaty::model::{PhysicalAddress, PhysicalRange};

const CAPABILITIES: usize = 0;
const DDTP: usize = 0x10;
const MODE: u64 = 0xf;
const BUSY: u64 = 1 << 4;
const PPN: u64 = ((1 << 44) - 1) << 10;

pub fn registers(node: Node<'_, '_>) -> Result<Option<PhysicalRange>, TreeError> {
    if !node.enabled()? || !node.compatible(b"riscv,iommu")? {
        return Ok(None);
    }
    if node.property(b"#iommu-cells")? != Some(&1u32.to_be_bytes()) {
        return Err(TreeError::Unsupported);
    }
    Ok(Some(node.register(0)?))
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Window,
    Version,
    Mode,
    Busy,
    Address,
    Rejected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Directory {
    Off,
    Bare,
    One,
    Two,
    Three,
}

impl Directory {
    fn decode(value: u64) -> Result<Self, Error> {
        match value & MODE {
            0 => Ok(Self::Off),
            1 => Ok(Self::Bare),
            2 => Ok(Self::One),
            3 => Ok(Self::Two),
            4 => Ok(Self::Three),
            _ => Err(Error::Mode),
        }
    }
}

#[derive(Debug)]
pub struct Profile {
    pub sv39: bool,
    pub debug_translation: bool,
    extended_context: bool,
    pub little_endian_tables: bool,
    pub address_bits: u32,
    pub directory: Directory,
}

/// # Safety
/// The exclusive window must map the actual little-endian register file.
pub unsafe fn probe(window: &mut DeviceWindow) -> Result<Profile, Error> {
    let caps = u64::from_le(unsafe { window.read_u64(CAPABILITIES) }.map_err(|_| Error::Window)?);
    if caps as u8 != 0x10 {
        return Err(Error::Version);
    }
    let value = unsafe { directory(window) }?;
    Ok(Profile {
        extended_context: caps & (1 << 22) != 0,
        debug_translation: caps & (1 << 31) != 0,
        little_endian_tables: u32::from_le(
            unsafe { window.read_u32(8) }.map_err(|_| Error::Window)?,
        ) & 1
            == 0,
        address_bits: ((caps >> 32) & 0x3f) as u32,
        sv39: caps & (1 << 9) != 0,
        directory: Directory::decode(value)?,
    })
}

unsafe fn directory(window: &mut DeviceWindow) -> Result<u64, Error> {
    let value = u64::from_le(unsafe { window.read_u64(DDTP) }.map_err(|_| Error::Window)?);
    if value & BUSY != 0 {
        return Err(Error::Busy);
    }
    Ok(value)
}

/// A pending register write retains the original window. Calling `observe`
/// again only samples DDTP. It never resubmits a command.
#[must_use]
pub struct Change<'a> {
    window: &'a mut DeviceWindow,
    target: u64,
    order: fn(),
}

impl Change<'_> {
    /// A matching WARL readback establishes the selected mode and table root.
    /// Off does not invalidate cached directory entries or exclude future replay.
    pub fn observe(&mut self) -> Result<Directory, Error> {
        (self.order)();
        let actual = unsafe { directory(self.window)? };
        let selected = Directory::decode(actual)?;
        if actual & MODE != self.target & MODE
            || (selected != Directory::Off && (actual ^ self.target) & PPN != 0)
        {
            return Err(Error::Rejected);
        }
        Ok(selected)
    }
}

/// # Safety
/// Control the actual controller and every requester affected by this change.
/// `order` must order MMIO writes against later reads and publish any prior
/// table stores to this device. Preserve old tables and independent retirement
/// obligations, including if this operation or its observer is abandoned.
pub unsafe fn shut(window: &mut DeviceWindow, order: fn()) -> Result<Change<'_>, Error> {
    let previous = unsafe { directory(window)? };
    Directory::decode(previous)?;
    // The PPN must remain unchanged on entry to Off. Preserve WPRI fields.
    let target = previous & !MODE;
    order();
    unsafe { window.write_u64(DDTP, target.to_le()) }.map_err(|_| Error::Window)?;
    Ok(Change {
        window,
        target,
        order,
    })
}

/// Install one directory level from Off. Hardware may reject an unsupported
/// depth or physical address, which is reported by `observe`.
///
/// # Safety
/// In addition to `shut`'s control/ordering contract, retain the correctly
/// sized, initialized and device-visible DDT and reachable tables through
/// confirmed retirement. Keep requesters isolated until routes, stale caches
/// and old transactions are independently settled.
pub unsafe fn install(
    window: &mut DeviceWindow,
    root: PhysicalAddress,
    order: fn(),
) -> Result<Change<'_>, Error> {
    let previous = unsafe { directory(window)? };
    if Directory::decode(previous)? != Directory::Off {
        return Err(Error::Mode);
    }
    let address = root.get();
    if !address.is_multiple_of(4096) || address >> 56 != 0 {
        return Err(Error::Address);
    }
    let target = (previous & !(PPN | MODE)) | (address >> 2) | 2;
    order();
    unsafe { window.write_u64(DDTP, target.to_le()) }.map_err(|_| Error::Window)?;
    Ok(Change {
        window,
        target,
        order,
    })
}

pub mod queue;
pub mod translation;
