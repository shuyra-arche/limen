#[cfg(target_arch = "riscv64")]
use super::DtbMemory;
use super::FirmwareMap;
use core::ops::Range;

/// Facts admitted on the major CPU. These carry no allocation authority.
pub struct Boot<'a> {
    hart: usize,
    device_tree: Option<Range<usize>>,
    firmware_map: Option<FirmwareMap<'a>>,
    #[cfg(target_arch = "riscv64")]
    memory: DtbMemory,
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

    #[cfg(target_arch = "riscv64")]
    pub fn dtb_memory(&self) -> &DtbMemory {
        &self.memory
    }

    #[cfg(not(target_arch = "riscv64"))]
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

#[cfg(target_arch = "riscv64")]
impl Boot<'static> {
    pub(crate) fn from_opensbi(hart: usize, device_tree: Range<usize>, memory: DtbMemory) -> Self {
        Self {
            hart,
            device_tree: Some(device_tree),
            firmware_map: None,
            memory,
        }
    }
}
