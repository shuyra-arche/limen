//! Exclusive mapped storage for one ordinary issued Allocation.

use crate::metal::{address::PhysicalRange, mmu::internal::access::MappedRange};

use super::{Allocation, AllocationId, Error};

/// The unique original Allocation paired with its exact exclusive mapping.
/// Neither a physical address nor a same-length slice can substitute for
/// the original mapped material.
#[must_use]
pub struct MappedStorage<'a> {
    allocation: Allocation,
    mapping: MappedRange<'a>,
}

impl<'a> MappedStorage<'a> {
    /// # Safety
    /// Both inputs must be valid independently. If their ranges match, the
    /// caller must establish that the mapping belongs to this exact issued
    /// Allocation, not a different generation at the same address. The
    /// original owner must preserve the actual mapping and exclusive-storage
    /// obligations through all derived uses and their settlement. Range
    /// equality proves geometry only, not identity or hardware lifetime.
    pub unsafe fn prepare(
        allocation: Allocation,
        mapping: MappedRange<'a>,
    ) -> Result<Self, (Allocation, MappedRange<'a>, Error)> {
        if allocation.physical() != mapping.physical() {
            return Err((allocation, mapping, Error::Range));
        }
        Ok(Self {
            allocation,
            mapping,
        })
    }

    pub fn id(&self) -> AllocationId {
        self.allocation.id()
    }

    pub fn physical(&self) -> PhysicalRange {
        self.mapping.physical()
    }

    /// Recover the two original inputs from a private preparation. This
    /// does not retire the mapping, the Allocation, or any hardware effect.
    pub fn into_parts(self) -> (Allocation, MappedRange<'a>) {
        (self.allocation, self.mapping)
    }

    pub(super) fn allocation(&self) -> &Allocation {
        &self.allocation
    }

    pub(super) fn mapping(&self) -> &MappedRange<'a> {
        &self.mapping
    }

    pub(super) fn mapping_mut(&mut self) -> &mut MappedRange<'a> {
        &mut self.mapping
    }
}
