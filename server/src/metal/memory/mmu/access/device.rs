//! A device mapping is not ordinary Rust byte storage.

use core::{marker::PhantomData, ptr};
use limen_treaty::model::{Alignment, InternalAddress, PhysicalRange};

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
    /// On successful geometry validation, the entire window must map the
    /// indicated physical bytes in order, with valid device attributes,
    /// privileged access and the required ownership. Keep that mapping and
    /// control live until all users and original hardware effects settle.
    /// A window does not itself pin a dynamically removable translation.
    pub unsafe fn admit(physical: PhysicalRange, base: InternalAddress) -> Result<Self, Error> {
        let length = physical.length().as_usize().ok_or(Error::Range)?;
        let base = usize::try_from(base.get()).map_err(|_| Error::Range)?;
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

    pub fn is_aligned(&self, alignment: Alignment) -> bool {
        InternalAddress::new(self.base as u64).is_aligned(alignment)
    }

    fn address(&self, offset: usize, width: usize) -> Result<usize, Error> {
        let address = self.base.checked_add(offset).ok_or(Error::Range)?;
        if offset.checked_add(width).ok_or(Error::Range)? > self.length
            || !address.is_multiple_of(width)
        {
            return Err(Error::Range);
        }
        Ok(address)
    }

    /// Geometry only. Does not prove that any register permits this access.
    pub fn supports_u32(&self, offset: usize) -> bool {
        self.address(offset, 4).is_ok()
    }

    /// # Safety
    /// If geometry permits the access, the actual register must accept this
    /// width and the caller must own the read and any destructive effect.
    /// The caller supplies the device's ordering and endianness contract.
    pub unsafe fn read_u8(&mut self, offset: usize) -> Result<u8, Error> {
        let address = self.address(offset, 1)?;
        Ok(unsafe { ptr::without_provenance::<u8>(address).read_volatile() })
    }

    /// # Safety
    /// If geometry permits the access, the caller owns the exact write and
    /// the device must accept this width and value. Ordering is caller-owned.
    pub unsafe fn write_u8(&mut self, offset: usize, value: u8) -> Result<(), Error> {
        let address = self.address(offset, 1)?;
        unsafe { ptr::without_provenance_mut::<u8>(address).write_volatile(value) };
        Ok(())
    }

    /// # Safety
    /// If geometry permits the access, the actual register must accept a
    /// naturally aligned 32-bit read. The caller owns all possible effects,
    /// ordering and interpretation of the native-endian result.
    pub unsafe fn read_u32(&mut self, offset: usize) -> Result<u32, Error> {
        let address = self.address(offset, 4)?;
        Ok(unsafe { ptr::without_provenance::<u32>(address).read_volatile() })
    }

    /// # Safety
    /// If geometry permits the access, the caller owns the exact write and
    /// the register accepts this native-endian width/value. The caller owns
    /// ordering, readback, and actual device completion.
    pub unsafe fn write_u32(&mut self, offset: usize, value: u32) -> Result<(), Error> {
        let address = self.address(offset, 4)?;
        unsafe { ptr::without_provenance_mut::<u32>(address).write_volatile(value) };
        Ok(())
    }

    /// Geometry only. Does not grant access to the register.
    pub fn supports_u64(&self, offset: usize) -> bool {
        self.address(offset, 8).is_ok()
    }

    /// # Safety
    /// The caller owns a naturally aligned 64-bit register read, its ordering
    /// and any effects. Its device endianness must be interpreted by the caller.
    pub unsafe fn read_u64(&mut self, offset: usize) -> Result<u64, Error> {
        let address = self.address(offset, 8)?;
        Ok(unsafe { ptr::without_provenance::<u64>(address).read_volatile() })
    }

    /// # Safety
    /// The caller owns this naturally aligned 64-bit register write and its
    /// effects. The device must accept this width, ordering and byte order.
    pub unsafe fn write_u64(&mut self, offset: usize, value: u64) -> Result<(), Error> {
        let address = self.address(offset, 8)?;
        unsafe { ptr::without_provenance_mut::<u64>(address).write_volatile(value) };
        Ok(())
    }
}
