//! Legacy second-level table encoding and native cache visibility.

use limen_treaty::model::PhysicalAddress;

/// Native legacy root, context and three-level second-level entries. The
/// caller owns the subordinate second-level translation tables.
#[derive(Clone, Copy)]
pub struct Format {
    pub(super) address_mask: u64,
    pub(super) domain_limit: u32,
}

impl Format {
    pub(super) fn page(self, physical: PhysicalAddress) -> Option<u64> {
        let address = physical.get();
        (address != 0 && address & !self.address_mask == 0).then_some(address)
    }

    pub fn table_entry(self, next: PhysicalAddress) -> Option<u64> {
        Some(self.page(next)? | 3)
    }

    pub fn read_only_leaf(self, page: PhysicalAddress) -> Option<u64> {
        Some(self.page(page)? | 1)
    }

    pub fn writable_leaf(self, page: PhysicalAddress) -> Option<u64> {
        Some(self.page(page)? | 3)
    }
}

/// Cache-line writeback and ordering for a native x86 table mapping.
pub struct Visibility {
    pub(super) line: usize,
}

impl Visibility {
    #[cfg(all(limen_target, target_arch = "x86_64"))]
    pub fn native() -> Option<Self> {
        let features = core::arch::x86_64::__cpuid(1);
        let line = ((features.ebx >> 8) & 0xff) as usize * 8;
        (features.edx & (1 << 19) != 0 && line.is_power_of_two()).then_some(Self { line })
    }

    pub(super) fn publish<T>(&self, value: &T) {
        let line = self.line;
        let pointer = core::ptr::from_ref(value).cast::<u8>();
        #[cfg(all(limen_target, target_arch = "x86_64"))]
        unsafe {
            use core::arch::x86_64::{_mm_clflush, _mm_mfence};
            for offset in (0..size_of_val(value)).step_by(line) {
                _mm_clflush(pointer.add(offset));
            }
            _mm_mfence();
        }
        #[cfg(not(all(limen_target, target_arch = "x86_64")))]
        {
            let _ = (line, pointer);
            unreachable!("VT-d table publication requires a native x86 target");
        }
    }
}
