//! Four-level IA-32e page tables. Level zero is the 4 KiB leaf.

#[cfg(all(limen_target, target_arch = "x86_64"))]
mod native;

use super::{LeafAccess, Privilege};
use limen_treaty::model::{InternalAddress, InternalRange, PhysicalAddress};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct Format {
    physical_bits: u8,
    gigabyte: bool,
}

/// Exact cache encoding selected by the original PAT. MTRR and alias
/// compatibility for the actual range remain the mapping owner's inputs.
fn pat_index(pat: u64, memory: super::Memory) -> Option<u64> {
    let encoding = match memory {
        super::Memory::Normal => 6, // Write-back.
        super::Memory::Device => 0, // Uncacheable.
    };
    (0u64..8).find(|i| ((pat >> (8 * i)) & 0xff) == encoding)
}

/// The raw privileged registers inherited by a new logical processor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Controls {
    pub cr0: usize,
    pub cr3: usize,
    pub cr4: usize,
    pub efer: u64,
}

impl Controls {
    pub fn four_level_untagged(self) -> bool {
        const LA57: usize = 1 << 12;
        const PCIDE: usize = 1 << 17;
        self.cr4 & (LA57 | PCIDE) == 0
    }

    /// Test the active non-global, write-protected, NX-enabled profile.
    /// Platform-specific physical address modifiers must already be excluded.
    pub fn format(self, physical_bits: u8, gigabyte: bool) -> Option<Format> {
        const WP_PG: usize = (1 << 16) | (1 << 31);
        const PAE: usize = 1 << 5;
        const PGE: usize = 1 << 7;
        const PKE: usize = 1 << 22;
        const CET: usize = 1 << 23;
        const PKS: usize = 1 << 24;
        const LMA_NXE: u64 = (1 << 10) | (1 << 11);
        (self.four_level_untagged()
            && self.cr0 & WP_PG == WP_PG
            && self.efer & LMA_NXE == LMA_NXE
            && self.cr4 & (PAE | PGE | PKE | CET | PKS) == PAE
            && (32..=52).contains(&physical_bits)
            && (self.cr3 as u64) < (1u64 << physical_bits))
            .then_some(Format {
                physical_bits,
                gigabyte,
            })
    }
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
            .then_some(raw | 3 | privilege.x86_bit())
    }

    /// An initialized leaf for an offline table. The caller supplies the
    /// matching PAT and physical memory type for the whole leaf.
    pub fn leaf(
        self,
        address: PhysicalAddress,
        level: usize,
        privilege: Privilege,
        access: LeafAccess,
        memory: super::Memory,
        pat: u64,
    ) -> Option<u64> {
        if level > 2
            || level == 2 && !self.gigabyte
            || memory == super::Memory::Device && access == LeafAccess::ReadExecute
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
        Some(raw | cache | large | privilege.x86_bit() | rights | 1)
    }
}

impl Privilege {
    const fn x86_bit(self) -> u64 {
        match self {
            Self::Kernel => 0,
            Self::User => 1 << 2,
        }
    }
}
