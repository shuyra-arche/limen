//! Supervisor leaf policy for the common Sv entry format.

use crate::metal::memory::mmu::{
    LeafAccess,
    riscv64::sv::{ACCESSED, DIRTY, EXECUTE, Mode, PAGE_BYTES, PageNumber, READ, WRITE},
};
use limen_treaty::model::PhysicalAddress;

impl Mode {
    /// Encode a branch using the base supervisor PTE format.
    pub fn branch(address: PhysicalAddress) -> Option<u64> {
        Some(PageNumber::new(address, PAGE_BYTES)?.branch())
    }

    /// Encode a supervisor-only leaf. Eager A/D bits permit use on Harts
    /// that fault instead of updating them in hardware.
    pub fn leaf(
        self,
        address: PhysicalAddress,
        level: usize,
        permission: LeafAccess,
    ) -> Option<u64> {
        let page = PageNumber::new(address, self.leaf_size(level)?.get())?;
        let access = match permission {
            LeafAccess::ReadOnly => READ,
            LeafAccess::ReadWrite => READ | WRITE | DIRTY,
            LeafAccess::ReadExecute => READ | EXECUTE,
        };
        Some(page.leaf(ACCESSED | access))
    }
}
