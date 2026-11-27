//! Physical pin geometry and checked redirection register indices.

use super::Failure;

const FIRST_ENTRY: u8 = 0x10;
pub(super) const PIN_LIMIT: usize = (256 - FIRST_ENTRY as usize) / 2;

#[derive(Clone, Copy)]
pub(super) struct Pin(u8);

impl Pin {
    pub(super) fn index(self) -> usize {
        usize::from(self.0)
    }

    pub(super) fn low_register(self) -> u8 {
        FIRST_ENTRY + self.0 * 2
    }

    pub(super) fn high_register(self) -> u8 {
        self.low_register() + 1
    }
}

#[derive(Clone, Copy)]
pub(super) struct Pins {
    first: u32,
    last: u8,
}

impl Pins {
    pub(super) fn probe(first: u32, version: u32) -> Option<Self> {
        // 82093AA/equivalent direct-format window, including QEMU's 0x20 profile.
        let last = (version >> 16) as u8;
        if !matches!(version as u8, 0x11 | 0x20) || usize::from(last) >= PIN_LIMIT {
            return None;
        }
        first.checked_add(u32::from(last))?;
        Some(Self { first, last })
    }

    pub(super) fn pin(self, gsi: u32) -> Result<Pin, Failure> {
        let pin = gsi
            .checked_sub(self.first)
            .and_then(|n| u8::try_from(n).ok());
        pin.filter(|&n| n <= self.last)
            .map(Pin)
            .ok_or(Failure::Scope)
    }
}
