//! Checked APLIC register representations. No hardware access or owner state.

use super::Mode;

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

#[cfg(test)]
#[path = "../../../../../../tests/metal/interrupt/riscv64/external/aplic/encoding.rs"]
mod tests;
