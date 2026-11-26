//! Geometry and base PTE encoding for RV64 supervisor paging.

use crate::metal::{address::PhysicalAddress, module::Permissions};

pub const PAGE_BYTES: u64 = 4096;
pub const TABLE_ENTRIES: usize = 512;
pub(super) const PPN_MASK: u64 = (1 << 44) - 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
#[repr(u8)]
pub enum Mode {
    Sv39 = 8,
    Sv48 = 9,
    Sv57 = 10,
}

impl Mode {
    /// Number of page-table levels. The base page is level zero.
    pub const fn levels(self) -> usize {
        self as usize - 5
    }

    pub const fn virtual_bits(self) -> u32 {
        12 + 9 * self.levels() as u32
    }

    pub const fn is_canonical(self, address: u64) -> bool {
        let shift = 64 - self.virtual_bits();
        ((address << shift) as i64 >> shift) as u64 == address
    }

    /// Leaf size in bytes, including superpages at the upper levels.
    pub const fn leaf_size(self, level: usize) -> Option<u64> {
        if level < self.levels() {
            Some(PAGE_BYTES << (9 * level))
        } else {
            None
        }
    }

    pub const fn index(self, address: u64, level: usize) -> Option<usize> {
        let Some(size) = self.leaf_size(level) else {
            return None;
        };
        if !self.is_canonical(address) {
            return None;
        }
        Some(((address / size) % TABLE_ENTRIES as u64) as usize)
    }

    /// Encode a branch using the base supervisor PTE format.
    pub fn branch(address: PhysicalAddress) -> Option<u64> {
        Some(page_number(address, PAGE_BYTES)? << 10 | 1)
    }

    /// Encode a supervisor-only leaf for module storage. Eager A/D bits
    /// permit use on Harts that fault instead of updating them in hardware.
    pub fn leaf(
        self,
        address: PhysicalAddress,
        level: usize,
        permission: Permissions,
    ) -> Option<u64> {
        let page = page_number(address, self.leaf_size(level)?)?;
        let flags = match permission {
            Permissions::ReadOnly => 0x43,
            Permissions::ReadWrite => 0xc7,
            Permissions::ReadExecute => 0x4b,
        };
        Some(page << 10 | flags)
    }
}

pub(super) fn page_number(address: PhysicalAddress, alignment: u64) -> Option<u64> {
    let address = address.get();
    let page = address / PAGE_BYTES;
    (page <= PPN_MASK && address.is_multiple_of(alignment)).then_some(page)
}
