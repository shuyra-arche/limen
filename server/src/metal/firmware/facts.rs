//! Facts retained from the firmware handoff.

use super::acpi::Acpi;
use super::{
    devicetree::{self, memory::DtbMemory},
    map::FirmwareMap,
};
use crate::metal::address::PhysicalRange;

pub(crate) struct Facts<'a> {
    pub(crate) image: PhysicalRange,
    pub(crate) firmware_map: Option<FirmwareMap<'a>>,
}

/// Retained firmware descriptions prepared in the startup caller's frame.
pub(crate) struct Descriptions<'a> {
    pub(crate) device_tree: Option<devicetree::DeviceTree<'a>>,
    pub(crate) memory: Option<DtbMemory>,
    // The parser bounds its retained table directory; a missing description
    // is a runtime fact, not a property of the selected ISA.
    pub(crate) acpi: Option<Acpi<'a>>,
}

impl Descriptions<'_> {
    pub(crate) const fn new() -> Self {
        Self {
            device_tree: None,
            memory: None,
            acpi: None,
        }
    }
}
