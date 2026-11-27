//! Physical register access and IOMMU feature discovery.

use super::{Directory, Error};
use crate::metal::{
    memory::access::treaty::device::DeviceWindow,
    platform::description::devicetree::{Error as TreeError, Node},
};
use limen_treaty::model::{ByteLength, PhysicalAddress, PhysicalRange};

const CAPABILITIES: usize = 0;
const CONTROL: usize = 8;
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

#[derive(Clone, Copy, Debug)]
pub struct Profile {
    pub sv39: bool,
    pub debug_translation: bool,
    pub(super) extended_context: bool,
    pub little_endian_tables: bool,
    pub address_bits: u32,
    pub directory: Directory,
}

/// Device and coherent-memory ordering supplied by the native Hart.
pub struct DeviceOrder(pub(super) fn());

impl DeviceOrder {
    #[cfg(all(limen_target, target_arch = "riscv64"))]
    pub fn native() -> Self {
        Self(|| unsafe { core::arch::asm!("fence iorw, iorw", options(nostack)) })
    }

    pub(super) fn fence(&self) {
        (self.0)();
    }
}

#[derive(Clone, Copy)]
pub(super) struct DirectoryWord(pub(super) u64);

impl DirectoryWord {
    pub(super) fn mode(self) -> Directory {
        Directory::decode(self.0).expect("checked directory mode")
    }

    pub(super) fn root(self) -> Option<PhysicalAddress> {
        match self.mode() {
            Directory::Off | Directory::Bare => None,
            Directory::One | Directory::Two | Directory::Three => {
                Some(PhysicalAddress::new((self.0 & PPN) << 2))
            }
        }
    }

    pub(super) fn off(self) -> Self {
        // The PPN must remain unchanged on entry to Off. Preserve WPRI fields.
        Self(self.0 & !MODE)
    }

    pub(super) fn one(self, root: PhysicalAddress, address_bits: u32) -> Result<Self, Error> {
        let address = root.get();
        if !address.is_multiple_of(4096) || address >> address_bits.min(56) != 0 {
            return Err(Error::Address);
        }
        Ok(Self((self.0 & !(PPN | MODE)) | (address >> 2) | 2))
    }

    pub(super) fn confirms(self, target: Self) -> bool {
        self.mode() == target.mode()
            && (target.mode() == Directory::Off || self.0 & PPN == target.0 & PPN)
    }
}

impl Directory {
    fn decode(word: u64) -> Result<Self, Error> {
        match word & MODE {
            0 => Ok(Self::Off),
            1 => Ok(Self::Bare),
            2 => Ok(Self::One),
            3 => Ok(Self::Two),
            4 => Ok(Self::Three),
            _ => Err(Error::Mode),
        }
    }
}

/// Owns one admitted register file and its required ordering.
pub(super) struct Registers<'a> {
    window: DeviceWindow<'a>,
    order: DeviceOrder,
    capabilities: u64,
    little_endian_tables: bool,
}

impl<'a> Registers<'a> {
    /// # Safety
    /// The window is the exclusive, actual little-endian IOMMU register
    /// mapping. All accesses and external users obey the controller contract.
    pub(super) unsafe fn admit(
        window: &'a mut DeviceWindow<'_>,
        order: DeviceOrder,
    ) -> Result<(Self, DirectoryWord), Error> {
        let caps =
            u64::from_le(unsafe { window.read_u64(CAPABILITIES) }.map_err(|_| Error::Window)?);
        if caps as u8 != 0x10 {
            return Err(Error::Version);
        }
        let control = u32::from_le(unsafe { window.read_u32(CONTROL) }.map_err(|_| Error::Window)?);
        let initial = unsafe { read_directory(window)? };
        Ok((
            Self {
                window: window.reborrow(),
                order,
                capabilities: caps,
                little_endian_tables: control & 1 == 0,
            },
            initial,
        ))
    }

    pub(super) fn profile(&self, directory: Directory) -> Profile {
        let caps = self.capabilities;
        Profile {
            extended_context: caps & (1 << 22) != 0,
            debug_translation: caps & (1 << 31) != 0,
            little_endian_tables: self.little_endian_tables,
            address_bits: self.address_bits(),
            sv39: caps & (1 << 9) != 0,
            directory,
        }
    }

    pub(super) fn address_bits(&self) -> u32 {
        ((self.capabilities >> 32) & 0x3f) as u32
    }

    pub(super) fn scope(&self) -> PhysicalRange {
        self.window.physical()
    }

    pub(super) fn order(&self) -> &DeviceOrder {
        &self.order
    }

    pub(super) fn write_directory(&mut self, target: DirectoryWord) -> Result<(), Error> {
        self.order.fence();
        unsafe { self.window.write_u64(DDTP, target.0.to_le()) }.map_err(|_| Error::Window)
    }

    pub(super) fn observe_directory(&mut self) -> Result<DirectoryWord, Error> {
        self.order.fence();
        unsafe { read_directory(&mut self.window) }
    }

    pub(super) fn debug_window(&mut self) -> Result<DeviceWindow<'_>, Error> {
        if self.capabilities & (1 << 31) == 0 {
            return Err(Error::Mode);
        }
        let physical = self
            .scope()
            .start()
            .advance(ByteLength::new(0x258))
            .and_then(|start| start.range(ByteLength::new(0x18)))
            .ok_or(Error::Window)?;
        self.window.subrange(physical).map_err(|_| Error::Window)
    }

    pub(super) fn queue_window(&mut self) -> (&mut DeviceWindow<'a>, &DeviceOrder) {
        (&mut self.window, &self.order)
    }
}

unsafe fn read_directory(window: &mut DeviceWindow<'_>) -> Result<DirectoryWord, Error> {
    let word = u64::from_le(unsafe { window.read_u64(DDTP) }.map_err(|_| Error::Window)?);
    if word & BUSY != 0 {
        return Err(Error::Busy);
    }
    Directory::decode(word)?;
    Ok(DirectoryWord(word))
}
