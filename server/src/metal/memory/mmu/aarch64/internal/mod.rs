//! EL1 stage-one, 4 KiB granule, 48-bit VA and up to 48-bit output addresses.
//! As in the other internal encoders, level zero denotes the base page.

#[cfg(all(limen_target, target_arch = "aarch64"))]
mod native;
#[cfg(all(limen_target, target_arch = "aarch64"))]
pub(in crate::metal::memory::mmu) use native::NativeRoot;
#[cfg(all(limen_target, target_arch = "aarch64"))]
pub use native::admit_root;

use crate::metal::memory::mmu::{LeafAccess, Memory, Privilege};
use limen_treaty::model::{InternalAddress, InternalRange, PhysicalAddress};

/// Actual EL1 translation registers, also inherited at secondary startup.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Controls {
    pub mair: u64,
    pub tcr: u64,
    pub ttbr0: u64,
    pub ttbr1: u64,
    pub sctlr: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Root {
    Lower,
    Upper,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Format {
    root: Root,
    physical_bits: u8,
    mair: u64,
}

impl Controls {
    /// Qualify one untagged 8-bit-ASID, little-endian EL1 root with Normal WB walks and one Inner Shareable domain.
    /// Non-secure EL1, baseline descriptor controls outside TCR_EL1,
    /// stage-one ownership and coherent walks are platform obligations.
    pub fn format(self, root: Root, mmfr0: u64) -> Option<Format> {
        const WALK: u64 = 1 << 8 | 1 << 10 | 3 << 12;
        // TnSZ, EPDn, cacheability, shareability and granule of each walk.
        const FIELDS: u64 = 0xffbf;
        let (shift, granule) = match root {
            Root::Lower => (0, 0),
            Root::Upper => (16, 2 << 14),
        };
        const IPS: u64 = 7 << 32;
        const ENDIAN: u64 = 3 << 24;
        let ips = ((self.tcr & IPS) >> 32) as usize;
        let physical_bits = *[32, 36, 40, 42, 44, 48].get(ips)?;
        let parange = mmfr0 & 15;
        let supported_granule = (mmfr0 >> 28) & 15;
        // Ignore no extension controls in TCR: new address/permission
        // interpretations need their own qualification.
        if self.tcr & !(IPS | FIELDS | (FIELDS << 16)) != 0
            || (self.tcr >> shift) & FIELDS != (16 | WALK | granule)
            || self.sctlr & (1 | ENDIAN) != 1
            || parange > 7
            || ips as u64 > parange
            || !matches!(supported_granule, 0 | 1)
        {
            return None;
        }
        Some(Format {
            root,
            physical_bits,
            mair: self.mair,
        })
    }
}

impl Format {
    /// The entire untagged address must belong to this root’s 48-bit region.
    pub fn is_canonical(self, address: InternalAddress) -> bool {
        let upper = match self.root {
            Root::Lower => 0,
            Root::Upper => 0xffff,
        };
        address.get() >> 48 == upper
    }

    pub fn is_canonical_range(self, range: InternalRange) -> bool {
        self.is_canonical(range.start())
            && self.is_canonical(InternalAddress::new(range.end().get() - 1))
    }

    pub fn index(self, address: InternalAddress, level: usize) -> Option<usize> {
        (level < 4 && self.is_canonical(address))
            .then(|| ((address.get() >> (12 + 9 * level)) & 0x1ff) as usize)
    }

    pub fn branch(self, address: PhysicalAddress) -> Option<u64> {
        let raw = address.get();
        (raw.is_multiple_of(4096) && raw < 1 << self.physical_bits).then_some(raw | 3)
    }

    /// Encode Normal WB or Device-nGnRnE using a matching real MAIR slot.
    /// Page paths and physical aliases must agree on the selected attributes.
    pub fn leaf(
        self,
        address: PhysicalAddress,
        level: usize,
        privilege: Privilege,
        access: LeafAccess,
        memory: Memory,
    ) -> Option<u64> {
        if level > 2 || memory == Memory::Device && access == LeafAccess::ReadExecute {
            return None;
        }
        let raw = address.get();
        if !raw.is_multiple_of(1 << (12 + 9 * level)) || raw >= 1 << self.physical_bits {
            return None;
        }
        let encoding = match memory {
            Memory::Normal => 0xff,
            Memory::Device => 0x00,
        };
        let attr = (0u64..8).find(|index| (self.mair >> (8 * index)) & 0xff == encoding)?;
        let mut entry = raw | (attr << 2) | (1 << 10);
        // SH=Inner Shareable for Normal WB; Device is always Outer Shareable.
        if memory == Memory::Normal {
            entry |= 3 << 8;
        } else {
            entry |= 2 << 8;
        }
        entry |= if level == 0 { 3 } else { 1 };
        const UXN: u64 = 1 << 54;
        const PXN: u64 = 1 << 53;
        if privilege == Privilege::User {
            entry |= (1 << 6) | (1 << 11) | PXN;
        } else {
            entry |= UXN;
        }
        if access != LeafAccess::ReadWrite {
            entry |= 1 << 7;
        }
        if access != LeafAccess::ReadExecute {
            entry |= PXN | UXN;
        }
        Some(entry)
    }
}
