//! Exclusive storage in a real, already established CPU mapping.

use core::{mem::MaybeUninit, ptr::NonNull, slice};

use crate::address::{PhysicalRange, length};
use limen_treaty::model::ByteLength;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Range,
    Split,
}

/// An exclusive byte borrow, not a physical allocation or a translation
/// authority. Its original mapping must remain valid through every derived use.
#[must_use]
pub struct MappedRange<'a> {
    physical: PhysicalRange,
    bytes: &'a mut [MaybeUninit<u8>],
}

impl<'a> MappedRange<'a> {
    /// # Safety
    /// The caller is the actual storage owner or its authorized borrower.
    /// If the geometric check succeeds, the entire slice must be ordinary
    /// writable RAM, with each byte
    /// corresponding to the supplied physical byte in order. The storage
    /// must satisfy Rust slice validity and exclusivity, including through
    /// every alias and any hardware access. Preserve the real mapping,
    /// compatible attributes and the backing material until all derived
    /// borrows and typed objects have ended. A numeric address or a
    /// successfully checked physical extent does not establish these facts.
    pub unsafe fn new(
        physical: PhysicalRange,
        bytes: &'a mut [MaybeUninit<u8>],
    ) -> Result<Self, Error> {
        if length(physical).as_usize() != Some(bytes.len()) {
            return Err(Error::Range);
        }
        Ok(Self { physical, bytes })
    }

    /// Borrow an original identity-mapped RAM extent from its raw storage pointer.
    /// Reject invalid geometry before forming a reference and retain the supplied
    /// pointer's provenance.
    ///
    /// # Safety
    /// On success, `base` must provide a single valid ordinary RAM storage range
    /// covering the complete physical extent. The actual owner must keep its
    /// writable identity mapping and compatible attributes live, and exclude
    /// conflicting software or hardware access for `'a`. A matching address
    /// does not prove any of these conditions.
    pub unsafe fn from_identity(
        physical: PhysicalRange,
        base: NonNull<MaybeUninit<u8>>,
    ) -> Result<Self, Error> {
        let start = usize::try_from(physical.start().get()).map_err(|_| Error::Range)?;
        let end = usize::try_from(physical.end().get()).map_err(|_| Error::Range)?;
        let length = end.checked_sub(start).ok_or(Error::Range)?;
        if length == 0 || length > isize::MAX as usize || base.addr().get() != start {
            return Err(Error::Range);
        }
        let bytes = unsafe { slice::from_raw_parts_mut(base.as_ptr(), length) };
        Ok(Self { physical, bytes })
    }

    pub fn physical(&self) -> PhysicalRange {
        self.physical
    }

    pub fn length(&self) -> usize {
        self.bytes.len()
    }

    pub fn bytes(&mut self) -> &mut [MaybeUninit<u8>] {
        self.bytes
    }

    pub(crate) fn address(&self) -> usize {
        self.bytes.as_ptr().addr()
    }

    pub(crate) fn into_bytes(self) -> &'a mut [MaybeUninit<u8>] {
        self.bytes
    }

    /// Consume one exclusive borrow and derive two disjoint borrows. This
    /// does not divide the source Allocation or create independent refund
    /// rights. Invalid offsets return the unchanged original borrow.
    pub fn split(self, offset: ByteLength) -> Result<(Self, Self), (Self, Error)> {
        let Some((left, right)) = self
            .physical
            .start()
            .advance(offset)
            .and_then(|mid| self.physical.split(mid))
        else {
            return Err((self, Error::Split));
        };
        let offset = offset.as_usize().expect("offset within the mapped slice");
        let (prefix, suffix) = self.bytes.split_at_mut(offset);
        Ok((
            Self {
                physical: left,
                bytes: prefix,
            },
            Self {
                physical: right,
                bytes: suffix,
            },
        ))
    }
}
