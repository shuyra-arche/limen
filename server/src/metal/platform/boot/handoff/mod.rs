//! Facts retained from the firmware handoff.

#[cfg(any(limen_firmware = "sbi", test))]
pub(crate) mod sbi;
#[cfg(any(limen_firmware = "uefi", test))]
pub(crate) mod uefi;

use crate::metal::platform::{
    description::{
        acpi::Acpi,
        devicetree::{self, memory::DtbMemory},
    },
    firmware::uefi::map::FirmwareMap,
};
use limen_treaty::model::PhysicalRange;

pub(crate) struct Facts<'a> {
    pub(crate) image: PhysicalRange,
    pub(crate) firmware_map: Option<FirmwareMap<'a>>,
}

/// Retained firmware descriptions prepared in the startup caller's frame.
pub(crate) struct Descriptions<'a> {
    #[cfg(limen_firmware = "sbi")]
    pub(crate) cpus: Option<crate::metal::processor::hart::riscv::description::RiscvCpus<'a>>,
    #[cfg(limen_firmware = "sbi")]
    pub(crate) sbi: Option<crate::metal::platform::firmware::sbi::SbiServices>,
    pub(crate) device_tree: Option<devicetree::DeviceTree<'a>>,
    pub(crate) memory: Option<DtbMemory>,
    // The parser bounds its retained table directory; a missing description
    // is a runtime fact, not a property of the selected ISA.
    pub(crate) acpi: Option<Acpi<'a>>,
}

impl Descriptions<'_> {
    pub(crate) const fn new() -> Self {
        Self {
            #[cfg(limen_firmware = "sbi")]
            cpus: None,
            #[cfg(limen_firmware = "sbi")]
            sbi: None,
            device_tree: None,
            memory: None,
            acpi: None,
        }
    }
}
