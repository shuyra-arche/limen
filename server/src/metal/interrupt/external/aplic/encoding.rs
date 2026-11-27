//! Checked APLIC register representations. No hardware access or owner state.

use super::{Mode, SOURCE_LIMIT};

const SIGNATURE: u32 = 0x80 << 24;
const INTERRUPT_ENABLE: u32 = 1 << 8;
const DELIVERY_MODE: u32 = 1 << 2;
const BIG_ENDIAN: u32 = 1;
const WRITABLE: u32 = INTERRUPT_ENABLE | DELIVERY_MODE | BIG_ENDIAN;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Delivery {
    Direct,
    Msi,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ByteOrder {
    Little,
    Big,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct DomainConfiguration {
    delivery: Delivery,
    enabled: bool,
    order: ByteOrder,
}

impl DomainConfiguration {
    pub(super) const fn shut() -> Self {
        Self {
            delivery: Delivery::Direct,
            enabled: false,
            order: ByteOrder::Little,
        }
    }

    pub(super) const fn open() -> Self {
        Self {
            enabled: true,
            ..Self::shut()
        }
    }

    // The current access path interprets words in little-endian order.
    // A different byte order is not admitted by the Direct owner.
    fn read(bits: u32) -> Option<Self> {
        if bits & !WRITABLE != SIGNATURE {
            return None;
        }
        Some(Self {
            delivery: if bits & DELIVERY_MODE == 0 {
                Delivery::Direct
            } else {
                Delivery::Msi
            },
            enabled: bits & INTERRUPT_ENABLE != 0,
            order: if bits & BIG_ENDIAN == 0 {
                ByteOrder::Little
            } else {
                ByteOrder::Big
            },
        })
    }

    pub(super) fn confirms(self, bits: u32) -> bool {
        Self::read(bits) == Some(self)
    }

    /// The fixed read-only signature is not part of a write request.
    pub(super) const fn write_bits(self) -> u32 {
        let enabled = if self.enabled { INTERRUPT_ENABLE } else { 0 };
        let delivery = match self.delivery {
            Delivery::Direct => 0,
            Delivery::Msi => DELIVERY_MODE,
        };
        let order = match self.order {
            ByteOrder::Little => 0,
            ByteOrder::Big => BIG_ENDIAN,
        };
        enabled | delivery | order
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SourceConfiguration {
    Inactive,
    Active(Mode),
}

impl SourceConfiguration {
    pub(super) const fn bits(self) -> u32 {
        match self {
            Self::Inactive => 0,
            Self::Active(Mode::EdgeRising) => 4,
            Self::Active(Mode::EdgeFalling) => 5,
            Self::Active(Mode::LevelHigh) => 6,
            Self::Active(Mode::LevelLow) => 7,
        }
    }

    pub(super) fn confirms(self, bits: u32) -> bool {
        bits == self.bits()
    }
}

const HART_INDEX_SHIFT: u32 = 18;
const HART_INDEX_LIMIT: usize = (1 << 14) - 1;

/// The supported Direct profile uses priority one; no MSI target is encoded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Target {
    index: u32,
}

impl Target {
    pub(super) const PRIORITY: u8 = 1;

    pub(super) fn new(index: usize) -> Option<Self> {
        (index <= HART_INDEX_LIMIT).then_some(Self {
            index: index as u32,
        })
    }

    pub(super) const fn bits(self) -> u32 {
        (self.index << HART_INDEX_SHIFT) | Self::PRIORITY as u32
    }

    pub(super) fn confirms(self, bits: u32) -> bool {
        bits == self.bits()
    }
}

const INTERRUPT_ID_SHIFT: u32 = 16;
const PRIORITY_MASK: u32 = 0xff;
const INTERRUPT_MASK: u32 = (SOURCE_LIMIT << INTERRUPT_ID_SHIFT) | PRIORITY_MASK;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Interrupt {
    Empty,
    Claimed { source: u32, priority: u8 },
}

impl Interrupt {
    /// Interpret one already-observed claim; never perform a second read.
    pub(super) fn read(raw: u32) -> Option<Self> {
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
#[path = "../../../../../tests/metal/interrupt/external/aplic/mod.rs"]
mod tests;
