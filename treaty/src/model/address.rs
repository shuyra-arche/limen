use super::{Alignment, ByteLength, PageCount, Range};

macro_rules! byte_coordinate {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(u64);

        impl $name {
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

            pub fn range(self, length: ByteLength) -> Option<Range<Self>> {
                Range::new(self, self.advance(length)?)
            }

            pub const fn is_aligned(self, alignment: Alignment) -> bool {
                alignment.is_aligned(ByteLength::new(self.0))
            }

            pub const fn align_down(self, alignment: Alignment) -> Self {
                Self(alignment.align_down(ByteLength::new(self.0)).get())
            }

            pub fn align_up(self, alignment: Alignment) -> Option<Self> {
                alignment
                    .align_up(ByteLength::new(self.0))
                    .map(|value| Self(value.get()))
            }
        }
    };
}

byte_coordinate!(
    /// Byte coordinate in an Internal address space.
    InternalAddress
);
byte_coordinate!(
    /// Byte coordinate in an External address space.
    ExternalAddress
);
byte_coordinate!(
    /// Byte coordinate relative to an object's original extent.
    ByteOffset
);

/// Logical page coordinate relative to a Flat's window.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PageOffset(u64);

impl PageOffset {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }

    pub fn advance(self, length: PageCount) -> Option<Self> {
        self.0.checked_add(length.get()).map(Self)
    }

    pub fn retreat(self, length: PageCount) -> Option<Self> {
        self.0.checked_sub(length.get()).map(Self)
    }

    pub fn distance(self, start: Self) -> Option<PageCount> {
        self.0.checked_sub(start.0).map(PageCount::new)
    }

    pub fn range(self, length: PageCount) -> Option<Range<Self>> {
        Range::new(self, self.advance(length)?)
    }
}
