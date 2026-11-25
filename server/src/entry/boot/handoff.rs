use super::{DtbMemory, FirmwareMap, FirmwareRegion, Physical, physical::Exclusions};
use core::ops::Range;

/// Facts admitted on the major CPU. These carry no allocation authority.
pub struct Boot<'a> {
    hart: usize,
    image: Physical,
    device_tree: Option<Range<usize>>,
    firmware_map: Option<FirmwareMap<'a>>,
    memory: Option<DtbMemory>,
}

impl<'a> Boot<'a> {
    pub fn hart(&self) -> usize {
        self.hart
    }

    pub fn image(&self) -> Physical {
        self.image
    }

    pub(super) fn exclusions_for(&self, major: Physical, trap: Physical) -> Exclusions<'_> {
        Exclusions::new(
            self.image(),
            major,
            trap,
            self.device_tree(),
            self.firmware_map(),
            self.dtb_memory(),
        )
    }

    pub fn device_tree(&self) -> Option<Range<usize>> {
        self.device_tree.clone()
    }

    pub fn firmware_map(&self) -> Option<FirmwareMap<'a>> {
        self.firmware_map
    }

    /// RAM and reservations from a DTB supplied as a memory description.
    pub fn dtb_memory(&self) -> Option<&DtbMemory> {
        self.memory.as_ref()
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

    #[cfg(not(target_arch = "riscv64"))]
    pub(crate) fn from_uefi(
        hart: usize,
        image: Physical,
        device_tree: Option<Range<usize>>,
        firmware_map: FirmwareMap<'a>,
        memory: Option<DtbMemory>,
    ) -> Self {
        Self {
            hart,
            image,
            device_tree,
            firmware_map: Some(firmware_map),
            memory,
        }
    }
}

#[cfg(any(target_arch = "riscv64", test))]
impl Boot<'static> {
    pub(crate) fn from_opensbi(
        hart: usize,
        image: Range<usize>,
        device_tree: Range<usize>,
        memory: DtbMemory,
    ) -> Self {
        Self {
            hart,
            image: super::physical::extent(image),
            device_tree: Some(device_tree),
            firmware_map: None,
            memory: Some(memory),
        }
    }
}
