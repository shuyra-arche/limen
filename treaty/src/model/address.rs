use core::{
    cmp::Ordering,
    fmt,
    hash::{Hash, Hasher},
    marker::PhantomData,
};

use super::{Alignment, ByteLength, PageCount, Range};

/// A byte coordinate of one kind. The kind is not an actual address-space identity.
#[repr(transparent)]
pub struct ByteCoordinate<Kind> {
    value: u64,
    kind: PhantomData<fn() -> Kind>,
}

impl<Kind> Copy for ByteCoordinate<Kind> {}

impl<Kind> Clone for ByteCoordinate<Kind> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<Kind> fmt::Debug for ByteCoordinate<Kind> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("ByteCoordinate")
            .field(&self.value)
            .finish()
    }
}

impl<Kind> PartialEq for ByteCoordinate<Kind> {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
    }
}

impl<Kind> Eq for ByteCoordinate<Kind> {}

impl<Kind> PartialOrd for ByteCoordinate<Kind> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<Kind> Ord for ByteCoordinate<Kind> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.value.cmp(&other.value)
    }
}

impl<Kind> Hash for ByteCoordinate<Kind> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.value.hash(state);
    }
}

impl<Kind> ByteCoordinate<Kind> {
    pub const fn new(value: u64) -> Self {
        Self {
            value,
            kind: PhantomData,
        }
    }

    pub const fn get(self) -> u64 {
        self.value
    }

    pub fn advance(self, length: ByteLength) -> Option<Self> {
        self.value.checked_add(length.get()).map(Self::new)
    }

    pub fn retreat(self, length: ByteLength) -> Option<Self> {
        self.value.checked_sub(length.get()).map(Self::new)
    }

    pub fn distance(self, start: Self) -> Option<ByteLength> {
        self.value.checked_sub(start.value).map(ByteLength::new)
    }

    pub fn range(self, length: ByteLength) -> Option<Range<Self>> {
        Range::new(self, self.advance(length)?)
    }

    pub const fn is_aligned(self, alignment: Alignment) -> bool {
        alignment.is_aligned(ByteLength::new(self.value))
    }

    pub const fn align_down(self, alignment: Alignment) -> Self {
        Self::new(alignment.align_down(ByteLength::new(self.value)).get())
    }

    pub fn align_up(self, alignment: Alignment) -> Option<Self> {
        alignment
            .align_up(ByteLength::new(self.value))
            .map(|value| Self::new(value.get()))
    }
}

/// Physical byte coordinates, not physical access authority.
pub enum Physical {}
/// CPU address-space byte coordinates.
pub enum Internal {}
/// I/O address-space byte coordinates.
pub enum External {}
/// Byte coordinates relative to an object's original extent.
pub enum Relative {}

pub type PhysicalAddress = ByteCoordinate<Physical>;
pub type InternalAddress = ByteCoordinate<Internal>;
pub type ExternalAddress = ByteCoordinate<External>;
pub type ByteOffset = ByteCoordinate<Relative>;

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
