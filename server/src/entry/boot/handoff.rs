use super::FirmwareMap;
#[cfg(not(target_arch = "riscv64"))]
use super::Loader;
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

    #[cfg(target_arch = "riscv64")]
    pub(super) unsafe fn admit(hart: usize, input: *const ()) -> Option<Self> {
        let tree = unsafe { super::tree::extent(input.cast(), crate::metal::boot::image()) }?;
        Some(Self {
            hart,
            device_tree: Some(tree),
            firmware_map: None,
        })
    }

    #[cfg(not(target_arch = "riscv64"))]
    pub(super) unsafe fn admit(hart: usize, input: *const ()) -> Option<Self> {
        // The trusted loader retains this Rust value, its map and its DTB.
        let loader = unsafe { input.cast::<Loader<'a>>().as_ref() }?;
        let map = loader.map;
        // EFI_MEMORY_DESCRIPTOR v1 has a 40-byte prefix and 8-byte alignment.
        if map.version != 1
            || map.stride < 40
            || !map.stride.is_multiple_of(8)
            || map.descriptors.is_empty()
            || !map.descriptors.as_ptr().addr().is_multiple_of(8)
            || !map.descriptors.len().is_multiple_of(map.stride)
        {
            return None;
        }
        let device_tree = match loader.device_tree {
            Some(bytes) => {
                let start = bytes.as_ptr().addr();
                let end = start.checked_add(bytes.len())?;
                if bytes.len() < 40 || !map.contains_loader_data(start..end) {
                    return None;
                }
                let extent = unsafe { super::tree::extent(bytes.as_ptr().cast(), 0..0) }?;
                if extent.end != end {
                    return None;
                }
                Some(extent)
            }
            None => None,
        };
        Some(Self {
            hart,
            device_tree,
            firmware_map: Some(map),
        })
    }
}
