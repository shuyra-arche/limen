//! Exclusive storage in a real, already established CPU mapping.

use core::mem::MaybeUninit;

use crate::entry::boot::Physical;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Range,
    Split,
}

/// An exclusive byte borrow, not a physical allocation or a translation
/// authority. Its original mapping must remain valid through every derived use.
#[must_use]
pub struct MappedRange<'a> {
    physical: Physical,
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
        physical: Physical,
        bytes: &'a mut [MaybeUninit<u8>],
    ) -> Result<Self, Error> {
        let length = physical.end.checked_sub(physical.start);
        if bytes.is_empty() || length.and_then(|n| usize::try_from(n).ok()) != Some(bytes.len()) {
            return Err(Error::Range);
        }
        Ok(Self { physical, bytes })
    }

    pub fn physical(&self) -> Physical {
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
    pub fn split(self, offset: usize) -> Result<(Self, Self), (Self, Error)> {
        if offset == 0 || offset >= self.length() {
            return Err((self, Error::Split));
        }
        let Some(mid) = u64::try_from(offset)
            .ok()
            .and_then(|offset| self.physical.start.checked_add(offset))
        else {
            return Err((self, Error::Split));
        };
        let Self { physical, bytes } = self;
        let (left, right) = bytes.split_at_mut(offset);
        Ok((
            Self {
                physical: Physical {
                    start: physical.start,
                    end: mid,
                },
                bytes: left,
            },
            Self {
                physical: Physical {
                    start: mid,
                    end: physical.end,
                },
                bytes: right,
            },
        ))
    }
}
