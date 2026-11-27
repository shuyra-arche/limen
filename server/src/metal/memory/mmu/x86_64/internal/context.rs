//! Copyable local CPU translation controls.

use super::format::Format;

/// The raw privileged registers inherited by a new logical processor.
/// This snapshot describes controls without granting installation authority.
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
            && self.cr3.is_multiple_of(4096)
            && (self.cr3 as u64) < (1u64 << physical_bits))
            .then_some(Format {
                physical_bits,
                gigabyte,
            })
    }
}
