#[cfg(not(target_arch = "riscv64"))]
use core::ops::Range;

/// Firmware descriptor bytes, with the stride and version from the final UEFI map.
#[derive(Clone, Copy)]
pub struct FirmwareMap<'a> {
    pub descriptors: &'a [u8],
    pub stride: usize,
    pub version: u32,
}

/// Loader-owned storage retained across the stack switch and Boot delivery.
/// The optional DTB is the exact, immutable copy in loader-data pages.
#[cfg(not(target_arch = "riscv64"))]
pub struct Loader<'a> {
    pub map: FirmwareMap<'a>,
    pub device_tree: Option<&'a [u8]>,
}

#[cfg(not(target_arch = "riscv64"))]
impl FirmwareMap<'_> {
    pub(crate) fn valid(&self) -> bool {
        // EFI_MEMORY_DESCRIPTOR v1 has a 40-byte prefix and 8-byte alignment.
        self.version == 1
            && self.stride >= 40
            && self.stride.is_multiple_of(8)
            && !self.descriptors.is_empty()
            && self.descriptors.as_ptr().addr().is_multiple_of(8)
            && self.descriptors.len().is_multiple_of(self.stride)
    }

    pub(crate) fn contains_loader_data(&self, tree: Range<usize>) -> bool {
        // Read the little-endian v1 fields at the firmware-reported stride.
        self.descriptors.chunks_exact(self.stride).any(|d| {
            let kind = u32::from_le_bytes(d[..4].try_into().unwrap());
            let base = u64::from_le_bytes(d[8..16].try_into().unwrap());
            let pages = u64::from_le_bytes(d[24..32].try_into().unwrap());
            kind == 2
                && base <= tree.start as u64
                && pages
                    .checked_mul(4096)
                    .and_then(|bytes| base.checked_add(bytes))
                    .is_some_and(|end| tree.end as u64 <= end)
        })
    }
}
