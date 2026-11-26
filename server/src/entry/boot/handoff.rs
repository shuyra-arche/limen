use limen_treaty::model::PhysicalRange;
use super::acpi::Acpi;
use super::{DtbMemory, FirmwareMap, FirmwareRegion, devicetree, physical::Exclusions};
use crate::metal::firmware::facts::Descriptions;
use core::ops::Range;

/// Facts admitted on the major CPU. These carry no allocation authority.
pub struct Boot<'a> {
    hart: usize,
    image: PhysicalRange,
    descriptions: &'a Descriptions<'a>,
    firmware_map: Option<FirmwareMap<'a>>,
}

impl<'a> Boot<'a> {
    pub fn hart(&self) -> usize {
        self.hart
    }

    pub fn image(&self) -> PhysicalRange {
        self.image
    }

    pub(crate) fn exclusions_for(
        &self,
        major: PhysicalRange,
        trap: PhysicalRange,
    ) -> Exclusions<'_> {
        Exclusions::new(
            self.image(),
            major,
            trap,
            self.device_tree(),
            self.firmware_map(),
            self.descriptions.memory.as_ref(),
        )
        .with_acpi(self.acpi())
    }

    pub fn device_tree(&self) -> Option<Range<usize>> {
        let bytes = self.descriptions.device_tree?.bytes().as_ptr_range();
        Some(bytes.start.addr()..bytes.end.addr())
    }

    /// The validated, retained hardware description in its original scope.
    pub fn device_tree_description(&self) -> Option<devicetree::DeviceTree<'a>> {
        self.descriptions.device_tree
    }

    pub fn acpi(&self) -> Option<&Acpi<'a>> {
        self.descriptions.acpi.as_ref()
    }

    pub fn firmware_map(&self) -> Option<FirmwareMap<'a>> {
        self.firmware_map
    }

    /// RAM and reservations from a DTB supplied as a memory description.
    pub fn dtb_memory(&self) -> Option<&DtbMemory> {
        self.descriptions
            .memory
            .as_ref()
            .filter(|memory| memory.ram().next().is_some())
    }

    /// Compare DTB RAM with the final UEFI map when both were supplied.
    /// The iterator gives unreserved whole pages corroborated by both
    /// descriptions. Live storage and access retirement still need checking.
    /// An empty iterator means the two descriptions yield no such pages.
    pub fn conventional_ram(&self) -> Option<impl Iterator<Item = FirmwareRegion> + '_> {
        Some(
            self.firmware_map
                .as_ref()?
                .conventional_ram(self.dtb_memory()?),
        )
    }

    pub(crate) fn new(
        hart: usize,
        image: PhysicalRange,
        firmware_map: Option<FirmwareMap<'a>>,
        descriptions: &'a Descriptions<'a>,
    ) -> Self {
        Self {
            hart,
            image,
            descriptions,
            firmware_map,
        }
    }
}
