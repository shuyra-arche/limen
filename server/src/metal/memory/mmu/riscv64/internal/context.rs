//! Values for RV64 ASID-zero translation contexts.

use super::format::Format;
use crate::metal::memory::mmu::riscv64::sv::{PAGE_BYTES, PageNumber};
use limen_treaty::model::PhysicalAddress;

/// A copyable SATP description for Bare or ASID-zero paging.
/// Installation requires the separately admitted local authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Context(pub(super) u64);

impl Context {
    pub const BARE: Self = Self(0);

    pub fn paged(mode: Format, root: PhysicalAddress) -> Option<Self> {
        Some(Self(
            (mode as u64) << 60 | PageNumber::new(root, PAGE_BYTES)?.get(),
        ))
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

    pub const fn mode(self) -> Option<Format> {
        match self.0 >> 60 {
            8 => Some(Format::Sv39),
            9 => Some(Format::Sv48),
            10 => Some(Format::Sv57),
            _ => None,
        }
    }

    pub fn root(self) -> Option<PhysicalAddress> {
        self.mode()
            .map(|_| PhysicalAddress::new((self.0 & PageNumber::MASK) * PAGE_BYTES))
    }
}
