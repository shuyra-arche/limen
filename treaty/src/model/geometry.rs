use core::num::NonZeroU64;

macro_rules! measure {
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

            pub const fn as_usize(self) -> Option<usize> {
                let value = self.0 as usize;
                if value as u64 == self.0 {
                    Some(value)
                } else {
                    None
                }
            }

            pub fn checked_add(self, other: Self) -> Option<Self> {
                self.0.checked_add(other.0).map(Self)
            }

            pub fn checked_sub(self, other: Self) -> Option<Self> {
                self.0.checked_sub(other.0).map(Self)
            }

            pub fn checked_mul(self, count: u64) -> Option<Self> {
                self.0.checked_mul(count).map(Self)
            }
        }
    };
}

measure!(
    /// A distance or size in bytes.
    ByteLength
);
measure!(
    /// A distance or size in logical pages.
    PageCount
);

/// The nonzero, power-of-two byte size of one page unit.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PageSize(NonZeroU64);

impl PageSize {
    pub const fn new(value: u64) -> Option<Self> {
        match NonZeroU64::new(value) {
            Some(size) if value.is_power_of_two() => Some(Self(size)),
            _ => None,
        }
    }

    pub const fn get(self) -> u64 {
        self.0.get()
    }

    pub const fn covering(self, length: ByteLength) -> PageCount {
        PageCount::new(length.get().div_ceil(self.get()))
    }

    pub fn exact(self, length: ByteLength) -> Option<PageCount> {
        let bytes = length.get();
        bytes
            .is_multiple_of(self.get())
            .then_some(PageCount::new(bytes / self.get()))
    }

    pub fn bytes(self, count: PageCount) -> Option<ByteLength> {
        self.get().checked_mul(count.get()).map(ByteLength::new)
    }
}

/// A nonzero, power-of-two byte alignment requirement.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Alignment(NonZeroU64);

impl Alignment {
    pub const fn new(value: u64) -> Option<Self> {
        match NonZeroU64::new(value) {
            Some(alignment) if value.is_power_of_two() => Some(Self(alignment)),
            _ => None,
        }
    }

    pub const fn get(self) -> u64 {
        self.0.get()
    }

    pub const fn is_aligned(self, length: ByteLength) -> bool {
        length.get().is_multiple_of(self.get())
    }

    pub const fn align_down(self, length: ByteLength) -> ByteLength {
        ByteLength::new(length.get() & !(self.get() - 1))
    }

    pub fn align_up(self, length: ByteLength) -> Option<ByteLength> {
        length
            .get()
            .checked_next_multiple_of(self.get())
            .map(ByteLength::new)
    }
}

/// A nonempty half-open interval within one coordinate kind.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Range<C> {
    start: C,
    end: C,
}

impl<C: Copy + Ord> Range<C> {
    pub fn new(start: C, end: C) -> Option<Self> {
        (start < end).then_some(Self { start, end })
    }

    pub const fn start(self) -> C {
        self.start
    }

    pub const fn end(self) -> C {
        self.end
    }

    pub fn contains(self, position: C) -> bool {
        self.start <= position && position < self.end
    }

    pub fn covers(self, other: Self) -> bool {
        self.start <= other.start && other.end <= self.end
    }

    pub fn overlaps(self, other: Self) -> bool {
        self.start < other.end && other.start < self.end
    }

    pub fn adjacent(self, other: Self) -> bool {
        self.end == other.start || other.end == self.start
    }

    pub fn intersect(self, other: Self) -> Option<Self> {
        Self::new(self.start.max(other.start), self.end.min(other.end))
    }

    pub fn split(self, at: C) -> Option<(Self, Self)> {
        Some((Self::new(self.start, at)?, Self::new(at, self.end)?))
    }

    pub fn merge(self, other: Self) -> Option<Self> {
        if self.end < other.start || other.end < self.start {
            return None;
        }
        Some(Self {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        })
    }
}
