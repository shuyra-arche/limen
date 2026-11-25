use super::{Boot, FirmwareMap, devicetree};

/// Loader-owned storage retained across the stack switch and Boot delivery.
/// The optional DTB is the exact, immutable copy in loader-data pages.
pub struct Loader<'a> {
    pub descriptors: &'a [u8],
    pub stride: usize,
    pub version: u32,
    pub descriptor_limit: usize,
    pub device_tree: Option<&'a [u8]>,
}

impl<'a> Loader<'a> {
    pub(crate) fn admit(&self, hart: usize) -> Option<Boot<'a>> {
        let map = FirmwareMap::new(
            self.descriptors,
            self.stride,
            self.version,
            self.descriptor_limit,
        )
        .ok()?;
        let device_tree = match self.device_tree {
            Some(bytes) => {
                let start = bytes.as_ptr().addr();
                let end = start.checked_add(bytes.len())?;
                let physical_start = u64::try_from(start).ok()?;
                let physical_end = u64::try_from(end).ok()?;
                // The loader retains the identity mapping and owns the DTB
                // copy. A valid map must cover its entire extent as loader data.
                if bytes.len() < 40
                    || !map.regions().any(|region| {
                        region.kind == 2
                            && region.physical.start <= physical_start
                            && physical_end <= region.physical.end
                    })
                {
                    return None;
                }
                let extent = unsafe { devicetree::extent(bytes.as_ptr().cast(), 0..0) }?;
                if extent.end != end {
                    return None;
                }
                Some(extent)
            }
            None => None,
        };
        Some(Boot::new(hart, device_tree, Some(map)))
    }
}
