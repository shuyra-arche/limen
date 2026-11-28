//! A device mapping is not ordinary Rust byte storage.

use core::{marker::PhantomData, ptr};
use limen_treaty::model::{Alignment, ByteLength, InternalAddress, PhysicalRange};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Range,
}

/// Actual mapping retention supplied by the owner of a removable mapping.
/// Borrowing a window keeps that original retention in place.
#[must_use]
pub struct DeviceMapping<R> {
    extent: Extent,
    retention: R,
    local: PhantomData<*mut ()>,
}

impl<R> DeviceMapping<R> {
    /// # Safety
    /// The retention must own the actual mapping of this region, including
    /// its backing, device attributes and permissions. No outside action may
    /// withdraw, replace or independently grant overlapping register access
    /// while retention is held. The controller must settle device effects
    /// before the caller relinquishes the retained mapping.
    pub unsafe fn admit(
        physical: PhysicalRange,
        base: InternalAddress,
        retention: R,
    ) -> Result<Self, (R, Error)> {
        match Extent::checked(physical, base) {
            Ok(extent) => Ok(Self {
                extent,
                retention,
                local: PhantomData,
            }),
            Err(error) => Err((retention, error)),
        }
    }

    pub fn window(&mut self) -> DeviceWindow<'_> {
        DeviceWindow::from_extent(self.extent)
    }

    /// Return the original retention after every window borrow has ended.
    pub fn into_retention(self) -> R {
        self.retention
    }
}

#[derive(Clone, Copy)]
struct Extent {
    physical: PhysicalRange,
    base: usize,
}

impl Extent {
    fn checked(physical: PhysicalRange, base: InternalAddress) -> Result<Self, Error> {
        let length = physical.length().as_usize().ok_or(Error::Range)?;
        let base = usize::try_from(base.get()).map_err(|_| Error::Range)?;
        if base == 0 || length == 0 || base.checked_add(length).is_none() {
            return Err(Error::Range);
        }
        Ok(Self { physical, base })
    }

    fn restrict(self, part: PhysicalRange) -> Result<Self, Error> {
        if !self.physical.covers(part) {
            return Err(Error::Range);
        }
        let displacement = part
            .start()
            .distance(self.physical.start())
            .and_then(|length| length.as_usize())
            .ok_or(Error::Range)?;
        Ok(Self {
            physical: part,
            base: self.base + displacement,
        })
    }
}

/// A checked exclusive view of device registers. The view never forms a RAM
/// slice. The register protocol, interconnect ordering and live device effects
/// remain the responsibility of the register's controller.
#[must_use]
pub struct DeviceWindow<'mapping> {
    extent: Extent,
    local: PhantomData<(&'mapping mut (), *mut ())>,
}

impl DeviceWindow<'static> {
    /// Admit an early, permanently resident platform device mapping.
    ///
    /// # Safety
    /// The indicated physical bytes must already be mapped in order with
    /// valid device attributes and privileged access. The trusted platform
    /// guarantees the mapping remains fixed and grants exclusive register
    /// access for every use derived from this window. A removable mapping
    /// requires a retention represented by a DeviceMapping.
    pub unsafe fn admit(physical: PhysicalRange, base: InternalAddress) -> Result<Self, Error> {
        Extent::checked(physical, base).map(Self::from_extent)
    }
}

impl<'mapping> DeviceWindow<'mapping> {
    fn from_extent(extent: Extent) -> Self {
        Self {
            extent,
            local: PhantomData,
        }
    }

    pub fn reborrow(&mut self) -> DeviceWindow<'_> {
        DeviceWindow::from_extent(self.extent)
    }

    /// Borrow a checked physical subrange without granting a second parent view.
    pub fn subrange(&mut self, physical: PhysicalRange) -> Result<DeviceWindow<'_>, Error> {
        self.extent
            .restrict(physical)
            .map(DeviceWindow::from_extent)
    }

    /// Consume the parent view and grant disjoint views with the same retention.
    /// An invalid boundary returns the original view.
    pub fn split(self, offset: ByteLength) -> Result<(Self, Self), (Self, Error)> {
        let Some((prefix, suffix)) = self
            .extent
            .physical
            .start()
            .advance(offset)
            .and_then(|at| self.extent.physical.split(at))
        else {
            return Err((self, Error::Range));
        };
        let left = self.extent.restrict(prefix).expect("left part covered");
        let right = self.extent.restrict(suffix).expect("right part covered");
        Ok((Self::from_extent(left), Self::from_extent(right)))
    }

    pub fn physical(&self) -> PhysicalRange {
        self.extent.physical
    }

    pub fn is_aligned(&self, alignment: Alignment) -> bool {
        InternalAddress::new(self.extent.base as u64).is_aligned(alignment)
    }

    fn address(&self, offset: usize, width: usize) -> Result<usize, Error> {
        let address = self.extent.base.checked_add(offset).ok_or(Error::Range)?;
        let length = self
            .extent
            .physical
            .length()
            .as_usize()
            .expect("validated length");
        if offset.checked_add(width).ok_or(Error::Range)? > length || !address.is_multiple_of(width)
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

    /// Observe a non-destructive 32-bit register without a mutable borrow.
    /// # Safety
    /// The original mapping and register must permit shared, non-destructive
    /// reads, including any interruptible reader. The caller owns the read
    /// protocol and excludes conflicting writes for the required lifetime.
    pub unsafe fn observe_u32(&self, offset: usize) -> Result<u32, Error> {
        let address = self.address(offset, 4)?;
        Ok(unsafe { ptr::without_provenance::<u32>(address).read_volatile() })
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

#[cfg(test)]
#[path = "../../../../../tests/metal/memory/access/partition.rs"]
mod tests;
