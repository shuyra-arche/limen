//! Observe physical ranges reported by the final firmware memory map.

pub(crate) use super::super::uefi::map::PAGE_ALIGNMENT;
use super::super::uefi::map::decode;
pub use super::super::uefi::map::{FirmwareMap, FirmwareRegion, MapError};

/// Firmware classification of an extent. Reclaimable storage still
/// belongs to its original users until their obligations are settled.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemoryUse {
    Conventional,
    Reclaimable,
    Reserved,
}

impl FirmwareRegion {
    pub fn memory_use(&self) -> MemoryUse {
        const CONVENTIONAL: u32 = 7;
        const RUNTIME: u64 = 1 << 63;
        match (self.kind, self.attributes & RUNTIME) {
            (CONVENTIONAL, 0) => MemoryUse::Conventional,
            // Loader and Boot Services code/data. ACPI reclaim also
            // requires ACPI enablement and is outside this handoff.
            (1..=4, 0) => MemoryUse::Reclaimable,
            _ => MemoryUse::Reserved,
        }
    }
}

impl FirmwareMap<'_> {
    pub fn regions(&self) -> impl ExactSizeIterator<Item = FirmwareRegion> + '_ {
        // The bytes are immutable and every prefix was checked at construction.
        self.descriptors()
            .chunks_exact(self.stride())
            .map(|entry| decode(entry).expect("validated firmware descriptor"))
    }

    pub fn conventional(&self) -> impl Iterator<Item = FirmwareRegion> + '_ {
        self.regions()
            .filter(|region| region.memory_use() == MemoryUse::Conventional)
    }
}
