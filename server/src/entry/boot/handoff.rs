use super::FirmwareMap;
use core::ops::Range;

/// Facts admitted on the major CPU. These carry no allocation authority.
pub struct Boot<'a> {
    hart: usize,
    device_tree: Option<Range<usize>>,
    firmware_map: Option<FirmwareMap<'a>>,
}

impl<'a> Boot<'a> {
    pub fn hart(&self) -> usize {
        self.hart
    }

    pub fn device_tree(&self) -> Option<Range<usize>> {
        self.device_tree.clone()
    }

    pub fn firmware_map(&self) -> Option<FirmwareMap<'a>> {
        self.firmware_map
    }

    pub(crate) fn new(
        hart: usize,
        device_tree: Option<Range<usize>>,
        firmware_map: Option<FirmwareMap<'a>>,
    ) -> Self {
        Self {
            hart,
            device_tree,
            firmware_map,
        }
    }
}
