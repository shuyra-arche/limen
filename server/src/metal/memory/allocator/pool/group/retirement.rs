use crate::metal::sync::{TurnMode, TurnWorkspace};
use limen_treaty::model::PhysicalRange;

use super::super::super::{
    Allocation, AllocationId, Allocator, ReturnId, Returned, returns::ReceiptOwner,
};
use super::super::{Error, GroupId, Pool, SourceError};
use super::{Chain, Group};

pub(in crate::metal::memory::allocator::pool) struct Material {
    pub(in crate::metal::memory::allocator::pool) allocation: Allocation,
    pub(in crate::metal::memory::allocator::pool) retirement: ReturnId,
}

impl Material {
    fn receipt(&self) -> Receipt {
        Receipt {
            source: self.allocation.source(),
            allocation: self.allocation.id(),
            physical: self.allocation.physical(),
            retirement: self.retirement,
        }
    }
}

#[derive(Clone, Copy)]
pub(in crate::metal::memory::allocator::pool) struct Receipt {
    source: &'static Allocator,
    allocation: AllocationId,
    physical: PhysicalRange,
    retirement: ReturnId,
}

pub(in crate::metal::memory::allocator::pool) enum GroupRecord<'storage, T> {
    Open(Group<'storage, T>),
    Shutting(Material),
    Returning(Receipt),
    Retired(Returned),
}

impl<T> GroupRecord<'_, T> {
    pub(in crate::metal::memory::allocator::pool) fn allocation(&self) -> AllocationId {
        match self {
            Self::Open(group) => group.material.allocation.id(),
            Self::Shutting(material) => material.allocation.id(),
            Self::Returning(receipt) => receipt.allocation,
            Self::Retired(result) => result.allocation,
        }
    }
}

/// Work rediscovered from the Pool's original directory.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Retirement {
    /// Original fresh preparation. Discovery confers no authority to close it.
    Preparing(GroupId),
    /// Check the original access evidence before beginning the physical return.
    Shutting(GroupId, PhysicalRange),
    /// The original source accepted the handoff. Its refund can be reconciled.
    Returning(GroupId),
    More,
    End,
}

impl<T> Pool<'_, T> {
    /// Advance a recovery pass by at most `budget` directory entries, capped
    /// at 64. A failed acquisition leaves the cursor intact. End resets it
    /// to revisit retained work. No mapped group bytes are accessed.
    pub fn retirement(&self, cursor: &mut usize, budget: usize) -> Result<Retirement, Error> {
        Ok(self.state.acquire(|state| {
            let stop = (*cursor).saturating_add(budget.min(64)).min(state.extent);
            while *cursor < stop {
                let index = *cursor;
                *cursor += 1;
                let slot = &state.groups[index];
                let Some(id) = slot.id(index) else { continue };
                let Some(entry) = slot.record.as_ref() else {
                    return Retirement::Preparing(id);
                };
                match entry {
                    GroupRecord::Shutting(material) => {
                        return Retirement::Shutting(id, material.allocation.physical());
                    }
                    GroupRecord::Returning(_) => return Retirement::Returning(id),
                    GroupRecord::Open(_) | GroupRecord::Retired(_) => {}
                }
            }
            if *cursor >= state.extent {
                *cursor = 0;
                Retirement::End
            } else {
                Retirement::More
            }
        })?)
    }

    /// Close an empty group to all future claims and withdraw its capacity.
    /// All old slot owners (including detached or reclaiming ones) must settle
    /// before the group can shut. The physical charge remains until refund.
    pub fn shut(&self, id: GroupId) -> Result<PhysicalRange, Error> {
        self.state.acquire(|state| {
            let capacity = match state.entry(id)? {
                GroupRecord::Open(group) if group.free == group.geometry.slots => {
                    group.geometry.slots
                }
                GroupRecord::Open(_) => return Err(Error::Busy),
                GroupRecord::Shutting(material) => return Ok(material.allocation.physical()),
                GroupRecord::Returning(receipt) => return Ok(receipt.physical),
                GroupRecord::Retired(result) => return Ok(result.physical),
            };
            state.unlink(id, Chain::Empty);
            let entry = &mut state.groups[id.0].record;
            let Some(GroupRecord::Open(group)) = entry.take() else {
                unreachable!("checked open group");
            };
            let physical = group.material.allocation.physical();
            *entry = Some(GroupRecord::Shutting(group.material));
            state.open -= 1;
            state.capacity -= capacity;
            state.free -= capacity;
            Ok(physical)
        })?
    }

    /// Handoff to the prepaid upstream Return. The original Allocation stays
    /// in its out-of-group record unless the Region accepts it. No physical
    /// refund occurs while the Pool Turn is held.
    ///
    /// # Safety
    ///
    /// The caller must have retired every applicable map, translation, device
    /// access and other use for the exact physical extent returned by shut on
    /// this group. This evidence must remain valid through the handoff.
    pub unsafe fn begin_return(&self, id: GroupId, mode: TurnMode) -> Result<(), Error> {
        let receipt = self.state.acquire(|state| match state.entry(id)? {
            GroupRecord::Open(_) => Err(Error::Busy),
            GroupRecord::Shutting(material) => Ok(Some(material.receipt())),
            GroupRecord::Returning(_) | GroupRecord::Retired(_) => Ok(None),
        })??;
        let Some(receipt) = receipt else {
            return Ok(());
        };

        let region = receipt.source.return_state(receipt.retirement)?;
        let mut work = TurnWorkspace::<2>::new();
        let mut plan = work.plan();
        self.plan(&mut plan).expect("Pool");
        plan.insert(region).expect("original Region");
        plan.acquire(mode, |held| {
            let (state, region) = held
                .access_pair(&self.state, region)
                .expect("complete return plan");
            let entry = state.entry(id)?;
            match entry {
                GroupRecord::Shutting(material) => {
                    // The fixed identity matches the original Allocation, and
                    // the caller proved its exact access lifecycle has ended.
                    let retirement =
                        unsafe { region.begin_retired(&material.allocation, ReceiptOwner::Pool) }?;
                    assert_eq!(retirement, receipt.retirement);
                    *entry = GroupRecord::Returning(receipt);
                    Ok(())
                }
                GroupRecord::Returning(_) | GroupRecord::Retired(_) => Ok(()),
                GroupRecord::Open(_) => unreachable!("a closed group never reopens"),
            }
        })
        .map_err(SourceError::from)?
    }

    /// Reconcile the group with its actual upstream refund. Failed attempts
    /// keep the exact receipt. Replays return the same settled result.
    pub fn finish_return(&self, id: GroupId, mode: TurnMode) -> Result<Returned, Error> {
        let receipt = match self.state.acquire(|state| match state.entry(id)? {
            GroupRecord::Returning(receipt) => Ok(Ok(*receipt)),
            GroupRecord::Retired(result) => Ok(Err(*result)),
            _ => Err(Error::Busy),
        })?? {
            Ok(receipt) => receipt,
            Err(done) => return Ok(done),
        };
        // The original Region and Allowance settle without any Pool lock.
        let done = match receipt.source.finish_return(receipt.retirement, mode) {
            Ok(done) => done,
            Err(error) => {
                // Another reclaimer can have finished and acknowledged the
                // Return after the observation above.
                return self.state.acquire(|state| match state.entry(id)? {
                    GroupRecord::Retired(done) => Ok(*done),
                    _ => Err(error.into()),
                })?;
            }
        };
        assert_eq!(done.allocation, receipt.allocation);
        assert_eq!(done.physical, receipt.physical);

        let region = receipt.source.return_state(receipt.retirement)?;
        let mut work = TurnWorkspace::<2>::new();
        let mut plan = work.plan();
        self.plan(&mut plan).expect("Pool");
        plan.insert(region).expect("original Region");
        plan.acquire(mode, |held| {
            let (state, region) = held
                .access_pair(&self.state, region)
                .expect("complete receipt plan");
            let entry = state.entry(id)?;
            match entry {
                GroupRecord::Retired(result) => return Ok(*result),
                GroupRecord::Returning(_) => {}
                _ => unreachable!("a handed-off group never reopens"),
            }
            region.acknowledge_return(receipt.retirement, ReceiptOwner::Pool)?;
            *entry = GroupRecord::Retired(done);
            state.bytes -= done
                .physical
                .length()
                .as_usize()
                .expect("installed group size");
            Ok(done)
        })
        .map_err(SourceError::from)?
    }
}
