use super::acpi::Acpi;
use super::{DtbMemory, FirmwareMap, FirmwareRegion, devicetree, physical::Exclusions};
use crate::metal::platform::boot::handoff::Descriptions;
#[cfg(limen_target)]
use crate::metal::platform::boot::major::Admission;
#[cfg(limen_cooperative)]
use crate::metal::processor::treaty::Major;
use core::ops::Range;
use limen_treaty::model::PhysicalRange;

/// Facts admitted on the major CPU. These carry no allocation authority.
pub struct Boot<'a> {
    hart: usize,
    image: PhysicalRange,
    descriptions: &'a Descriptions<'a>,
    firmware_map: Option<FirmwareMap<'a>>,
    #[cfg(limen_target)]
    terminal: crate::metal::processor::treaty::Terminal,
    #[cfg(limen_target)]
    stack: Range<usize>,
    #[cfg(limen_cooperative)]
    major: Option<Major>,
    #[cfg(all(limen_target, target_arch = "riscv64"))]
    reception: Option<crate::metal::interrupt::riscv64::internal::supervisor::Supervisor>,
}

impl<'a> Boot<'a> {
    pub fn hart(&self) -> usize {
        self.hart
    }

    pub fn image(&self) -> PhysicalRange {
        self.image
    }

    /// The terminal operations supplied by the trusted platform handoff.
    #[cfg(limen_target)]
    pub const fn terminal(&self) -> crate::metal::processor::treaty::Terminal {
        self.terminal
    }

    /// The ordinary stack supplied by the trusted image for the major CPU.
    #[cfg(limen_target)]
    pub fn stack_range(&self) -> Range<usize> {
        self.stack.clone()
    }

    /// Transfer the original native construction relation to its consumer.
    #[cfg(limen_cooperative)]
    pub fn take_major(&mut self) -> Option<Major> {
        self.major.take()
    }

    #[cfg_attr(
        not(any(limen_target, test)),
        expect(dead_code, reason = "native entry supplies the original stack ranges")
    )]
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
    pub fn riscv_cpus(&self) -> &crate::metal::processor::description::riscv64::cpu::RiscvCpus<'a> {
        self.descriptions
            .cpus
            .as_ref()
            .expect("admitted SBI CPU scope")
    }

    #[cfg(limen_firmware = "sbi")]
    pub fn sbi_services(&self) -> crate::metal::platform::firmware::treaty::sbi::SbiServices {
        self.descriptions.sbi.expect("admitted SBI services")
    }

    /// Move the major Hart's exclusive reception lines out of the handoff.
    #[cfg(all(limen_target, target_arch = "riscv64"))]
    pub fn take_reception(
        &mut self,
    ) -> Option<crate::metal::interrupt::riscv64::internal::supervisor::Supervisor> {
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

    #[cfg(limen_target)]
    pub(super) fn from_major(admission: Admission<'a>, descriptions: &'a Descriptions<'a>) -> Self {
        Self::new(
            admission.hart,
            admission.facts.image,
            admission.facts.firmware_map,
            descriptions,
            admission.terminal,
            admission.stack,
            #[cfg(limen_cooperative)]
            admission.major,
            #[cfg(target_arch = "riscv64")]
            admission.reception,
        )
    }

    #[cfg_attr(
        not(any(limen_target, test)),
        expect(dead_code, reason = "native entry delivers admitted boot data")
    )]
    pub(crate) fn new(
        hart: usize,
        image: PhysicalRange,
        firmware_map: Option<FirmwareMap<'a>>,
        descriptions: &'a Descriptions<'a>,
        #[cfg(limen_target)] terminal: crate::metal::processor::treaty::Terminal,
        #[cfg(limen_target)] stack: Range<usize>,
        #[cfg(limen_cooperative)] major: Major,
        #[cfg(all(limen_target, target_arch = "riscv64"))]
        reception: crate::metal::interrupt::riscv64::internal::supervisor::Supervisor,
    ) -> Self {
        Self {
            hart,
            image,
            descriptions,
            firmware_map,
            #[cfg(limen_target)]
            terminal,
            #[cfg(limen_target)]
            stack,
            #[cfg(limen_cooperative)]
            major: Some(major),
            #[cfg(all(limen_target, target_arch = "riscv64"))]
            reception: Some(reception),
        }
    }
}
