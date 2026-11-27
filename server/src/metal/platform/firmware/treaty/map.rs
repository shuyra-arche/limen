//! Observe physical ranges reported by the final firmware memory map.

pub(crate) use super::super::uefi::map::PAGE_ALIGNMENT;
use super::super::uefi::map::decode;
pub use super::super::uefi::map::{FirmwareMap, FirmwareRegion, MapError};

const CONVENTIONAL_MEMORY: u32 = 7;
const RUNTIME: u64 = 1 << 63;

impl FirmwareMap<'_> {
    pub fn regions(&self) -> impl ExactSizeIterator<Item = FirmwareRegion> + '_ {
        // The bytes are immutable and every prefix was checked at construction.
        self.descriptors()
            .chunks_exact(self.stride())
            .map(|entry| decode(entry).expect("validated firmware descriptor"))
    }

    pub fn conventional(&self) -> impl Iterator<Item = FirmwareRegion> + '_ {
        self.regions()
            .filter(|region| region.kind == CONVENTIONAL_MEMORY && region.attributes & RUNTIME == 0)
    }
}
