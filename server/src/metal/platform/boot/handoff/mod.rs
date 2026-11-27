//! Facts retained from the firmware handoff.

#[cfg_attr(
    not(any(limen_firmware = "sbi", test)),
    expect(dead_code, reason = "the SBI handoff supplies the original DTB window")
)]
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

#[cfg_attr(
    not(any(limen_target, test)),
    expect(dead_code, reason = "native entry consumes the admitted image and map")
)]
pub(crate) struct Facts<'a> {
    pub(crate) image: PhysicalRange,
    pub(crate) firmware_map: Option<FirmwareMap<'a>>,
    #[cfg(limen_target)]
    pub(crate) map_storage: Option<PhysicalRange>,
}

/// Retained firmware descriptions prepared in the startup caller's frame.
pub(crate) struct Descriptions<'a> {
    #[cfg(limen_firmware = "sbi")]
    pub(crate) cpus: Option<crate::metal::processor::description::riscv64::cpu::RiscvCpus<'a>>,
    #[cfg(limen_firmware = "sbi")]
    pub(crate) sbi: Option<crate::metal::platform::firmware::treaty::sbi::SbiServices>,
    pub(crate) device_tree: Option<devicetree::DeviceTree<'a>>,
    pub(crate) device_tree_physical: Option<PhysicalRange>,
    pub(crate) memory: Option<DtbMemory>,
    // The parser bounds its retained table directory; a missing description
    // is a runtime fact, not a property of the selected ISA.
    pub(crate) acpi: Option<Acpi<'a>>,
}

impl Descriptions<'_> {
    #[cfg_attr(
        not(any(limen_target, test)),
        expect(
            dead_code,
            reason = "native entry keeps the original descriptions in its frame"
        )
    )]
    pub(crate) const fn new() -> Self {
        Self {
            #[cfg(limen_firmware = "sbi")]
            cpus: None,
            #[cfg(limen_firmware = "sbi")]
            sbi: None,
            device_tree: None,
            device_tree_physical: None,
            memory: None,
            acpi: None,
        }
    }
}

// The boot protocol's trusted entry mapping establishes the physical
// correspondence. Callers must use the original identity-mapped extent.
#[cfg(any(limen_target, test))]
pub(super) unsafe fn identity_extent(range: core::ops::Range<usize>) -> Option<PhysicalRange> {
    use limen_treaty::model::PhysicalAddress;

    PhysicalRange::new(
        PhysicalAddress::new(range.start as u64),
        PhysicalAddress::new(range.end as u64),
    )
}
