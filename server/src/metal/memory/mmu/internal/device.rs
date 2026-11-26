//! A device mapping is not ordinary Rust byte storage.

use core::marker::PhantomData;
use limen_treaty::model::PhysicalRange;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Range,
}

/// A checked window in an existing device mapping. No RAM slice is formed.
/// Its actual mapping and exclusive device access remain caller obligations.
#[must_use]
pub struct DeviceWindow {
    physical: PhysicalRange,
    base: usize,
    length: usize,
    local: PhantomData<*mut ()>,
}

impl DeviceWindow {
    /// # Safety
    /// The entire window maps the indicated physical bytes in order with
    /// valid device attributes, privileged access and the actual ownership
    /// required for all derived accesses. Keep the mapping and control live
    /// until all users and original hardware effects have settled.
    pub unsafe fn admit(physical: PhysicalRange, base: usize) -> Result<Self, Error> {
        let length = physical.length().as_usize().ok_or(Error::Range)?;
        if base == 0 || length == 0 || base.checked_add(length).is_none() {
            return Err(Error::Range);
        }
        Ok(Self {
            physical,
            base,
            length,
            local: PhantomData,
        })
    }

    pub fn physical(&self) -> PhysicalRange {
        self.physical
    }

    pub fn base(&self) -> usize {
        self.base
    }

    /// Validate one aligned word access, without performing hardware I/O.
    pub fn word(&self, offset: usize) -> Option<usize> {
        let address = self.base.checked_add(offset)?;
        (offset.checked_add(4)? <= self.length && address.is_multiple_of(4))
            .then_some(address)
    }
}
