//! Values for RV64 ASID-zero translation contexts.

#[cfg(all(limen_target, target_arch = "riscv64"))]
mod satp;
#[cfg(all(limen_target, target_arch = "riscv64"))]
pub use satp::{SwitchError, Translation};

use super::{
    Mode, PAGE_BYTES,
    mode::{PPN_MASK, page_number},
};
use limen_treaty::model::PhysicalAddress;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Context(u64);

impl Context {
    pub const BARE: Self = Self(0);

    pub fn paged(mode: Mode, root: PhysicalAddress) -> Option<Self> {
        Some(Self((mode as u64) << 60 | page_number(root, PAGE_BYTES)?))
    }

    /// Import a Bare or ASID-zero supervisor context.
    pub fn from_bits(bits: u64) -> Option<Self> {
        let context = Self(bits);
        (context == Self::BARE || (context.mode().is_some() && bits & (0xffff << 44) == 0))
            .then_some(context)
    }

    pub const fn bits(self) -> u64 {
        self.0
    }

    pub const fn mode(self) -> Option<Mode> {
        match self.0 >> 60 {
            8 => Some(Mode::Sv39),
            9 => Some(Mode::Sv48),
            10 => Some(Mode::Sv57),
            _ => None,
        }
    }

    pub fn root(self) -> Option<PhysicalAddress> {
        self.mode()
            .map(|_| PhysicalAddress::new((self.0 & PPN_MASK) * PAGE_BYTES))
    }
}
