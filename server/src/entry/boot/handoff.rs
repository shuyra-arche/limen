use super::tree;
use core::ops::Range;

/// Facts from the admitted firmware handoff. These carry no authority over RAM.
pub struct Boot {
    hart: usize,
    device_tree: Range<usize>,
}

impl Boot {
    /// The loader must keep the DTB storage readable and stable for validation.
    pub(crate) unsafe fn from_device_tree(
        hart: usize,
        address: usize,
        image: Range<usize>,
    ) -> Option<Self> {
        let header = core::ptr::without_provenance(address);
        let device_tree = unsafe { tree::extent(header, image) }?;
        Some(Self { hart, device_tree })
    }

    pub(crate) fn enter(self) -> ! {
        unsafe { limen_platform_ready(self) }
    }

    pub fn hart(&self) -> usize {
        self.hart
    }

    /// Physical address interval occupied by the firmware device tree.
    pub fn device_tree(&self) -> Range<usize> {
        self.device_tree.clone()
    }
}

unsafe extern "Rust" {
    fn limen_platform_ready(boot: Boot) -> !;
}
