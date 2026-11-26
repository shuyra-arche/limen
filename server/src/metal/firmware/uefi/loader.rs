use super::{Boot, Descriptions, FirmwareMap, acpi, devicetree};
use crate::address::{PhysicalAddress, PhysicalRange};

/// How the trusted platform uses its retained DTB.
pub enum DeviceTree<'a> {
    /// An immutable hardware description retained for device discovery.
    Description(&'a [u8]),
    /// A memory description to check against the final UEFI map.
    Memory(&'a [u8]),
}

/// An ACPI configuration-table location and independent trusted read access.
pub struct AcpiInput<'a> {
    pub rsdp: PhysicalAddress,
    /// Return exactly the requested stable bytes from this physical range.
    /// Keep the original storage reserved throughout the returned borrow.
    pub read: &'a dyn Fn(PhysicalRange) -> Option<&'a [u8]>,
}

const LOADER_DATA: u32 = 2;
const ACPI_RECLAIM: u32 = 9;
const ACPI_NVS: u32 = 10;

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
    pub acpi: Option<AcpiInput<'a>>,
}

impl<'a> Loader<'a> {
    pub(crate) fn admit<'b>(
        &self,
        hart: usize,
        descriptions: &'b mut Descriptions<'b>,
    ) -> Option<Boot<'b>>
    where
        'a: 'b,
    {
        let map = FirmwareMap::new(
            self.descriptors,
            self.stride,
            self.version,
            self.descriptor_limit,
        )
        .ok()?;
        descriptions.device_tree = match self.device_tree.as_ref() {
            Some(tree) => {
                let bytes = match tree {
                    DeviceTree::Description(bytes) | DeviceTree::Memory(bytes) => *bytes,
                };
                if !bytes.as_ptr().addr().is_multiple_of(8) {
                    return None;
                }
                let pointers = bytes.as_ptr_range();
                let physical = PhysicalRange::new(
                    PhysicalAddress::new(pointers.start.addr() as u64),
                    PhysicalAddress::new(pointers.end.addr() as u64),
                )?;
                if !map
                    .regions()
                    .any(|region| region.kind == LOADER_DATA && region.physical.covers(physical))
                {
                    return None;
                }
                let description = devicetree::DeviceTree::new(bytes).ok()?;
                match tree {
                    DeviceTree::Memory(_) => {
                        devicetree::memory::describe(description, &mut descriptions.memory)
                    }
                    DeviceTree::Description(_) => devicetree::memory::describe_reservations(
                        description,
                        &mut descriptions.memory,
                    ),
                }
                .ok()?;
                Some(description)
            }
            None => None,
        };
        if let Some(input) = &self.acpi {
            descriptions.acpi = Some(
                acpi::Acpi::new(input.rsdp, |physical| {
                    if !map.regions().any(|region| {
                        matches!(region.kind, ACPI_RECLAIM | ACPI_NVS)
                            && region.physical.covers(physical)
                    }) {
                        return None;
                    }
                    (input.read)(physical)
                })
                .ok()?,
            );
        }
        Some(Boot::new(hart, self.image, Some(map), descriptions))
    }
}
