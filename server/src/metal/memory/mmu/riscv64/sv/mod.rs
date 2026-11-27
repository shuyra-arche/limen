//! Base RISC-V Sv geometry and encoding shared by supervisor and I/O translation.

use limen_treaty::model::{InternalAddress, InternalRange, PageSize, PhysicalAddress};

pub const PAGE_BYTES: u64 = 4096;
pub const TABLE_ENTRIES: usize = 512;

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
}

/// The physical page number common to Sv page-table descriptors.
pub(super) struct PageNumber(u64);

impl PageNumber {
    pub(super) const MASK: u64 = (1 << 44) - 1;

    pub(super) fn new(address: PhysicalAddress, alignment: u64) -> Option<Self> {
        let raw = address.get();
        let page = raw / PAGE_BYTES;
        (page <= Self::MASK && raw.is_multiple_of(alignment)).then_some(Self(page))
    }

    pub(super) const fn get(self) -> u64 {
        self.0
    }

    pub(super) const fn branch(self) -> u64 {
        self.0 << 10 | VALID
    }

    pub(super) const fn leaf(self, flags: u64) -> u64 {
        self.branch() | flags
    }
}

const VALID: u64 = 1;
pub(super) const READ: u64 = 1 << 1;
pub(super) const WRITE: u64 = 1 << 2;
pub(super) const EXECUTE: u64 = 1 << 3;
pub(super) const ACCESSED: u64 = 1 << 6;
pub(super) const DIRTY: u64 = 1 << 7;
