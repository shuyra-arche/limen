//! Direct-delivery target and claim words.

use super::super::SOURCE_LIMIT;

const HART_INDEX_SHIFT: u32 = 18;
const HART_INDEX_LIMIT: usize = (1 << 14) - 1;

/// The supported Direct profile uses priority one; no MSI target is encoded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::metal::interrupt::riscv64::external::aplic) struct Target {
    index: u32,
}

impl Target {
    pub(in crate::metal::interrupt::riscv64::external::aplic) const PRIORITY: u8 = 1;

    pub(in crate::metal::interrupt::riscv64::external::aplic) fn new(index: usize) -> Option<Self> {
        (index <= HART_INDEX_LIMIT).then_some(Self {
            index: index as u32,
        })
    }

    pub(in crate::metal::interrupt::riscv64::external::aplic) const fn bits(self) -> u32 {
        (self.index << HART_INDEX_SHIFT) | Self::PRIORITY as u32
    }

    pub(in crate::metal::interrupt::riscv64::external::aplic) fn confirms(self, bits: u32) -> bool {
        bits == self.bits()
    }
}

const INTERRUPT_ID_SHIFT: u32 = 16;
const PRIORITY_MASK: u32 = 0xff;
const INTERRUPT_MASK: u32 = (SOURCE_LIMIT << INTERRUPT_ID_SHIFT) | PRIORITY_MASK;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::metal::interrupt::riscv64::external::aplic) enum Interrupt {
    Empty,
    Claimed { source: u32, priority: u8 },
}

impl Interrupt {
    /// Interpret one already-observed claim; never perform a second read.
    pub(in crate::metal::interrupt::riscv64::external::aplic) fn read(raw: u32) -> Option<Self> {
        if raw == 0 {
            return Some(Self::Empty);
        }
        if raw & !INTERRUPT_MASK != 0 {
            return None;
        }
        let source = (raw >> INTERRUPT_ID_SHIFT) & SOURCE_LIMIT;
        let priority = (raw & PRIORITY_MASK) as u8;
        (source != 0 && priority != 0).then_some(Self::Claimed { source, priority })
    }
}

#[cfg(test)]
#[path = "../../../../../../../tests/metal/interrupt/riscv64/external/aplic/direct/encoding.rs"]
mod tests;
