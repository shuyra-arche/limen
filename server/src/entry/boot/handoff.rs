use super::acpi::Acpi;
use super::{DtbMemory, FirmwareMap, FirmwareRegion, devicetree, physical::Exclusions};
use crate::metal::platform::boot::handoff::Descriptions;
use core::ops::Range;
use limen_treaty::model::PhysicalRange;

/// Facts admitted on the major CPU. These carry no allocation authority.
pub struct Boot<'a> {
    hart: usize,
    image: PhysicalRange,
    descriptions: &'a Descriptions<'a>,
    firmware_map: Option<FirmwareMap<'a>>,
    #[cfg(limen_firmware = "sbi")]
    reception: Option<crate::metal::interrupt::internal::supervisor::Supervisor>,
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

    /// CPU scope already checked at the SBI handoff.
    #[cfg(limen_firmware = "sbi")]
    pub fn riscv_cpus(
        &self,
    ) -> &crate::metal::processor::hart::riscv64::description::RiscvCpus<'a> {
        self.descriptions
            .cpus
            .as_ref()
            .expect("admitted SBI CPU scope")
    }

    #[cfg(limen_firmware = "sbi")]
    pub fn sbi_services(&self) -> crate::metal::platform::firmware::sbi::SbiServices {
        self.descriptions.sbi.expect("admitted SBI services")
    }

    /// Move the major Hart's exclusive reception lines out of the handoff.
    #[cfg(limen_firmware = "sbi")]
    pub fn take_reception(
        &mut self,
    ) -> Option<crate::metal::interrupt::internal::supervisor::Supervisor> {
        self.reception.take()
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
        Some(super::regions::conventional_ram(
            self.firmware_map.as_ref()?,
            self.dtb_memory()?,
        ))
    }

    pub(crate) fn new(
        hart: usize,
        image: PhysicalRange,
        firmware_map: Option<FirmwareMap<'a>>,
        descriptions: &'a Descriptions<'a>,
        #[cfg(limen_firmware = "sbi")]
        reception: crate::metal::interrupt::internal::supervisor::Supervisor,
    ) -> Self {
        Self {
            hart,
            image,
            descriptions,
            firmware_map,
            #[cfg(limen_firmware = "sbi")]
            reception: Some(reception),
        }
    }
}
