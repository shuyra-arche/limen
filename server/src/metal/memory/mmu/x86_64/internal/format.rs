//! Four-level IA-32e page tables. Level zero is the 4 KiB leaf.

use crate::metal::memory::mmu::treaty::{LeafAccess, Memory, Privilege};
use limen_treaty::model::{InternalAddress, InternalRange, PhysicalAddress};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct Format {
    pub(super) physical_bits: u8,
    pub(super) gigabyte: bool,
}

/// Exact cache encoding selected by the original PAT. MTRR and alias
/// compatibility for the actual range remain the mapping owner's inputs.
fn pat_index(pat: u64, memory: Memory) -> Option<u64> {
    let encoding = match memory {
        Memory::Normal => 6, // Write-back.
        Memory::Device => 0, // Uncacheable.
    };
    (0u64..8).find(|i| ((pat >> (8 * i)) & 0xff) == encoding)
}

impl Format {
    pub fn is_canonical(self, address: InternalAddress) -> bool {
        let raw = address.get();
        ((raw << 16) as i64 >> 16) as u64 == raw
    }

    pub fn is_canonical_range(self, range: InternalRange) -> bool {
        let first = range.start();
        let last = InternalAddress::new(range.end().get() - 1);
        self.is_canonical(first) && self.is_canonical(last) && first.get() >> 63 == last.get() >> 63
    }

    pub fn index(self, address: InternalAddress, level: usize) -> Option<usize> {
        (level < 4 && self.is_canonical(address))
            .then(|| ((address.get() >> (12 + 9 * level)) & 0x1ff) as usize)
    }

    pub fn branch(self, address: PhysicalAddress, privilege: Privilege) -> Option<u64> {
        let raw = address.get();
        (raw.is_multiple_of(4096) && raw < (1u64 << self.physical_bits))
            .then_some(raw | 3 | privilege_bit(privilege))
    }

    /// An initialized leaf for an offline table. The caller supplies the
    /// matching PAT and physical memory type for the whole leaf.
    pub fn leaf(
        self,
        address: PhysicalAddress,
        level: usize,
        privilege: Privilege,
        access: LeafAccess,
        memory: Memory,
        pat: u64,
    ) -> Option<u64> {
        if level > 2
            || level == 2 && !self.gigabyte
            || memory == Memory::Device && access == LeafAccess::ReadExecute
        {
            return None;
        }
        let raw = address.get();
        if !raw.is_multiple_of(1 << (12 + 9 * level)) || raw >= 1u64 << self.physical_bits {
            return None;
        }
        let index = pat_index(pat, memory)?;
        let cache = ((index & 3) << 3) | ((index >> 2) << if level == 0 { 7 } else { 12 });
        let large = if level == 0 { 0 } else { 1 << 7 };
        let rights = match access {
            LeafAccess::ReadOnly => 1u64 << 63,
            LeafAccess::ReadWrite => (1u64 << 63) | 2,
            LeafAccess::ReadExecute => 0,
        };
        Some(raw | cache | large | privilege_bit(privilege) | rights | 1)
    }
}

const fn privilege_bit(privilege: Privilege) -> u64 {
    match privilege {
        Privilege::Kernel => 0,
        Privilege::User => 1 << 2,
    }
}
