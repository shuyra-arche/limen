//! Shared RISC-V Sv page geometry and page-table entry fields.

use limen_treaty::model::{PageSize, PhysicalAddress};

pub(super) const PAGE_BYTES: u64 = 4096;
const TABLE_ENTRIES: u64 = 512;

pub(super) const fn level_size(level: usize) -> Option<PageSize> {
    if level < 5 {
        PageSize::new(PAGE_BYTES << (9 * level))
    } else {
        None
    }
}

pub(super) const fn index(address: u64, size: PageSize) -> usize {
    ((address / size.get()) % TABLE_ENTRIES) as usize
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
pub(super) const USER: u64 = 1 << 4;
pub(super) const ACCESSED: u64 = 1 << 6;
pub(super) const DIRTY: u64 = 1 << 7;
