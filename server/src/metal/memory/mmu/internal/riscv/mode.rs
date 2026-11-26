//! Geometry and base PTE encoding for RV64 supervisor paging.

use limen_treaty::model::{InternalAddress, InternalRange, PageSize, PhysicalAddress};

pub const PAGE_BYTES: u64 = 4096;
pub const TABLE_ENTRIES: usize = 512;
pub(super) const PPN_MASK: u64 = (1 << 44) - 1;

/// The access modes of the current supervisor-only leaf encoding contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LeafAccess {
    ReadOnly,
    ReadWrite,
    ReadExecute,
}

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

    pub const fn is_canonical(self, address: InternalAddress) -> bool {
        let address = address.get();
        let shift = 64 - self.virtual_bits();
        ((address << shift) as i64 >> shift) as u64 == address
    }

    /// Every occupied byte must be in one canonical half. The exclusive
    /// endpoint itself need not be a canonical address.
    pub const fn is_canonical_range(self, range: InternalRange) -> bool {
        let first = range.start();
        let last = InternalAddress::new(range.end().get() - 1);
        self.is_canonical(first) && self.is_canonical(last) && first.get() >> 63 == last.get() >> 63
    }

    /// Leaf size, including superpages at the upper levels.
    pub const fn leaf_size(self, level: usize) -> Option<PageSize> {
        if level < self.levels() {
            PageSize::new(PAGE_BYTES << (9 * level))
        } else {
            None
        }
    }

    pub const fn index(self, address: InternalAddress, level: usize) -> Option<usize> {
        let Some(size) = self.leaf_size(level) else {
            return None;
        };
        if !self.is_canonical(address) {
            return None;
        }
        Some(((address.get() / size.get()) % TABLE_ENTRIES as u64) as usize)
    }

    /// Encode a branch using the base supervisor PTE format.
    pub fn branch(address: PhysicalAddress) -> Option<u64> {
        Some(page_number(address, PAGE_BYTES)? << 10 | 1)
    }

    /// Encode a supervisor-only leaf. Eager A/D bits
    /// permit use on Harts that fault instead of updating them in hardware.
    pub fn leaf(
        self,
        address: PhysicalAddress,
        level: usize,
        permission: LeafAccess,
    ) -> Option<u64> {
        let page = page_number(address, self.leaf_size(level)?.get())?;
        let flags = match permission {
            LeafAccess::ReadOnly => 0x43,
            LeafAccess::ReadWrite => 0xc7,
            LeafAccess::ReadExecute => 0x4b,
        };
        Some(page << 10 | flags)
    }
}

pub(super) fn page_number(address: PhysicalAddress, alignment: u64) -> Option<u64> {
    let address = address.get();
    let page = address / PAGE_BYTES;
    (page <= PPN_MASK && address.is_multiple_of(alignment)).then_some(page)
}
