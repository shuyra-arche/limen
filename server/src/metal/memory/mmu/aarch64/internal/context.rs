//! Copyable local CPU translation controls.

use super::format::{Format, Root};

/// Actual EL1 translation registers, also inherited at secondary startup.
/// This snapshot describes controls without granting installation authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Controls {
    pub mair: u64,
    pub tcr: u64,
    pub ttbr0: u64,
    pub ttbr1: u64,
    pub sctlr: u64,
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
