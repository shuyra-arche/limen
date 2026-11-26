use super::{Boot, FirmwareMap, devicetree};
use crate::address::{PhysicalAddress, PhysicalRange};

/// How the trusted platform uses its retained DTB.
pub enum DeviceTree<'a> {
    /// A hardware description whose extent is checked at handoff.
    Description(&'a [u8]),
    /// A memory description to check against the final UEFI map.
    Memory(&'a [u8]),
}

const LOADER_DATA: u32 = 2;

/// Loader-owned storage retained across the stack switch and Boot delivery.
/// The optional DTB is the exact, immutable copy in loader-data pages.
pub struct Loader<'a> {
    /// Physical extent of the resident image under the entry identity map.
    pub image: PhysicalRange,
    pub descriptors: &'a [u8],
    pub stride: usize,
    pub version: u32,
    pub descriptor_limit: usize,
    pub device_tree: Option<DeviceTree<'a>>,
}

impl<'a> Loader<'a> {
    pub(crate) fn admit(&self, hart: usize) -> Option<Boot<'a>> {
        let map = FirmwareMap::new(
            self.descriptors,
            self.stride,
            self.version,
            self.descriptor_limit,
        )
        .ok()?;
        let mut memory = None;
        let device_tree = match self.device_tree.as_ref() {
            Some(tree) => {
                let bytes = match tree {
                    DeviceTree::Description(bytes) | DeviceTree::Memory(bytes) => *bytes,
                };
                let start = bytes.as_ptr().addr();
                let end = start.checked_add(bytes.len())?;
                let physical = PhysicalRange::new(
                    PhysicalAddress::new(u64::try_from(start).ok()?),
                    PhysicalAddress::new(u64::try_from(end).ok()?),
                )?;
                // The loader retains the identity mapping and owns the DTB
                // copy. A valid map must cover its entire extent as loader data.
                if bytes.len() < devicetree::HEADER_SIZE
                    || !map.regions().any(|region| {
                        region.kind == LOADER_DATA && region.physical.covers(physical)
                    })
                {
                    return None;
                }
                let extent = unsafe { devicetree::extent(bytes.as_ptr().cast(), 0..0) }?;
                if extent.end != end {
                    return None;
                }
                if let DeviceTree::Memory(_) = tree {
                    devicetree::memory::describe(bytes, &mut memory).ok()?;
                }
                Some(extent)
            }
            None => None,
        };
        Some(Boot::from_uefi(hart, self.image, device_tree, map, memory))
    }
}
