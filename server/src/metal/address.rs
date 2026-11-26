//! Physical coordinates shared by the hardware and boot owners.

use limen_treaty::model::{Alignment, ByteLength, Range};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct PhysicalAddress(u64);

pub type PhysicalRange = Range<PhysicalAddress>;

impl PhysicalAddress {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }

    pub fn advance(self, length: ByteLength) -> Option<Self> {
        self.0.checked_add(length.get()).map(Self)
    }

    pub fn retreat(self, length: ByteLength) -> Option<Self> {
        self.0.checked_sub(length.get()).map(Self)
    }

    pub fn distance(self, start: Self) -> Option<ByteLength> {
        self.0.checked_sub(start.0).map(ByteLength::new)
    }

    pub fn span(self, length: ByteLength) -> Option<PhysicalRange> {
        PhysicalRange::new(self, self.advance(length)?)
    }

    pub fn is_aligned(self, alignment: Alignment) -> bool {
        alignment.is_aligned(ByteLength::new(self.0))
    }

    pub fn align_down(self, alignment: Alignment) -> Self {
        Self(alignment.align_down(ByteLength::new(self.0)).get())
    }

    pub fn align_up(self, alignment: Alignment) -> Option<Self> {
        alignment
            .align_up(ByteLength::new(self.0))
            .map(|address| Self(address.get()))
    }
}

/// Compute the byte extent of a checked physical range.
pub fn length(range: PhysicalRange) -> ByteLength {
    range
        .end()
        .distance(range.start())
        .expect("checked physical range")
}
