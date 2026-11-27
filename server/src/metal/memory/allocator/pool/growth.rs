//! Prepaid directory and source construction for a fresh group.

use limen_treaty::model::PhysicalRange;

use super::super::{AllocationConstruction, MappedStorage};
use super::{Directory, Error, Geometry, GroupId, GroupReceipt, Pool};

/// One resident preparation supplied outside the group it will construct.
pub struct Growth {
    pub(super) physical: PhysicalRange,
    pub(super) receiver: AllocationConstruction,
}

impl Growth {
    pub const fn new(physical: PhysicalRange) -> Self {
        Self {
            physical,
            receiver: AllocationConstruction::within(physical),
        }
    }

    fn bytes(&self) -> Result<usize, Error> {
        self.physical.length().as_usize().ok_or(Error::Layout)
    }
}

impl<T> Directory<'_, T> {
    fn available(&self, bytes: usize) -> Result<(), Error> {
        self.limit
            .checked_sub(self.bytes)
            .and_then(|left| left.checked_sub(self.preparing))
            .filter(|left| *left >= bytes)
            .map(|_| ())
            .ok_or(Error::Directory)
    }

    fn growth(&self, id: GroupId) -> Result<&'static Growth, Error> {
        let slot = self.groups.get(id.0).ok_or(Error::Stale)?;
        if slot.record.is_some() || slot.id(id.0) != Some(id) {
            return Err(Error::Stale);
        }
        slot.growth.ok_or(Error::Stale)
    }

    pub(super) fn admission(
        &self,
        prepared: Option<GroupId>,
        bytes: usize,
    ) -> Result<Option<&'static Growth>, Error> {
        if let Some(id) = prepared {
            let growth = self.growth(id)?;
            if growth.bytes()? != bytes {
                return Err(Error::Material);
            }
            return Ok(Some(growth));
        }
        self.free_group.ok_or(Error::Directory)?;
        self.available(bytes)?;
        Ok(None)
    }
}

impl<'storage, T> Pool<'storage, T> {
    /// Claim a real directory position and charge its byte ceiling before
    /// releasing the range-bound original receiver to the spending owner.
    /// Errors leave the unique preparation in its input position.
    pub fn prepare(
        &self,
        input: &mut Option<&'static mut Growth>,
    ) -> Result<(GroupId, &'static AllocationConstruction), Error> {
        let proposed = input.as_deref().ok_or(Error::Receiver)?;
        let bytes = proposed.bytes()?;
        let geometry = Geometry::of::<T>(bytes)?;
        if !proposed
            .physical
            .start()
            .get()
            .is_multiple_of(geometry.alignment as u64)
        {
            return Err(Error::Layout);
        }
        self.state.acquire(|state| {
            let index = state.free_group.ok_or(Error::Directory)?;
            state.available(bytes)?;
            let growth: &'static Growth = input.take().expect("exclusive growth preparation");
            let slot = &mut state.groups[index];
            state.free_group = slot.next.take();
            slot.growth = Some(growth);
            state.extent = state.extent.max(index + 1);
            state.preparing += bytes;
            Ok((slot.id(index).expect("prepaid group"), &growth.receiver))
        })?
    }

    /// Reconcile an empty preparation after the original receiver is closed
    /// and the source has settled any reservation. This fact is monotonic.
    pub fn discard_empty(&self, id: GroupId) -> Result<(), Error> {
        let growth = self.state.acquire(|state| state.growth(id))??;
        if !growth.receiver.closed_empty()? {
            return Err(Error::Busy);
        }
        let bytes = growth.bytes()?;
        self.state.acquire(|state| {
            state.growth(id)?;
            let slot = &mut state.groups[id.0];
            slot.growth = None;
            slot.next = state.free_group;
            state.free_group = Some(id.0);
            state.preparing -= bytes;
            Ok(())
        })?
    }

    /// Publish only material issued through this exact prepaid preparation.
    ///
    /// # Safety
    /// The caller must satisfy the mapping, exclusivity and receipt lifetime
    /// requirements of `install` through preparation, use and retirement.
    pub unsafe fn install_prepared(
        &self,
        id: GroupId,
        material: &mut Option<MappedStorage<'storage>>,
        receipt: &mut Option<GroupReceipt>,
    ) -> Result<GroupId, Error> {
        unsafe { self.install_at(Some(id), material, receipt) }
    }
}
