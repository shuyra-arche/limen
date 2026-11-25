//! Exact physical occupation and settlement. The Region's two tables have
//! distinct scopes and lifetimes even when a historical return names an old
//! allocation whose active slot has been reused.

use core::ptr;

use crate::{
    entry::boot::Physical,
    metal::identity::{GenerationTable, Key, ReserveError},
    sync::{Acquisition, HeldLockSet, LockPlan, TurnLock, Work},
};

use super::{
    Allocator, AllowanceId, Candidate, Error, RegionId, allowance::SlotState, length, range,
    region::State, with_turn,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct AllocationScope(pub(super) usize, pub(super) usize);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ReturnScope(pub(super) usize, pub(super) usize);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AllocationId {
    region: RegionId,
    key: Key<AllocationScope>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReturnId {
    region: RegionId,
    key: Key<ReturnScope>,
}

#[must_use]
pub struct Allocation {
    source: &'static Allocator,
    id: AllocationId,
    physical: Physical,
}

/// Ownership and completed retirement of the exact original material.
#[must_use]
pub struct Retired(Allocation);

impl Allocation {
    pub(super) fn issued(source: &'static Allocator, id: AllocationId, physical: Physical) -> Self {
        Self {
            source,
            id,
            physical,
        }
    }

    pub fn id(&self) -> AllocationId {
        self.id
    }

    pub fn region(&self) -> RegionId {
        self.id.region
    }

    pub fn physical(&self) -> Physical {
        self.physical
    }

    pub(super) fn source(&self) -> &'static Allocator {
        self.source
    }

    /// The prepaid Return location belongs to the upstream source's table.
    /// Learning its identity does not authorize handoff or completion.
    pub(super) fn return_position(&self) -> Result<ReturnId, Error> {
        let region = self.source.region(self.id.region)?;
        with_turn(&region.state, |state| {
            state.active(self).map(|record| record.retirement)
        })?
    }

    /// Conclude the consumer's actual access lifecycle.
    ///
    /// # Safety
    ///
    /// The exact allocation must be closed to new use. Every applicable
    /// software access, CPU translation, device access and in-flight use must
    /// be retired. All proofs must remain valid through handoff. Owning this
    /// value, an address or a closed spending permission proves none of these.
    pub unsafe fn retire(self) -> Retired {
        Retired(self)
    }
}

impl Retired {
    pub fn physical(&self) -> Physical {
        self.0.physical
    }
}

#[derive(Clone, Copy)]
pub(super) enum Phase {
    Reserved(&'static TurnLock<Construction>),
    Issued,
    Returning,
}

#[derive(Clone, Copy)]
pub(super) struct AllocationRecord {
    physical: Physical,
    allowance: AllowanceId,
    retirement: ReturnId,
    phase: Phase,
}

#[derive(Clone, Copy)]
pub(super) enum ReturnRecord {
    Reserved(AllocationId),
    Pending(AllocationId, AllowanceId, ReceiptOwner),
    Done(Returned, ReceiptOwner),
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum ReceiptOwner {
    Caller,
    Pool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Returned {
    pub allocation: AllocationId,
    pub physical: Physical,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Pending {
    Found(ReturnId),
    More,
    End,
}

#[derive(Clone, Copy)]
struct Reservation {
    source: &'static Allocator,
    id: AllocationId,
    allowance: AllowanceId,
    candidate: Candidate,
}

enum Preparation {
    Open,
    Reserved(Reservation),
    Closed(Option<Reservation>),
    Issued(Option<Allocation>),
}

/// An actual consumer's persistent, single-use preparation and receiving
/// position. The consumer keeps the canonical Turn lock stable and can
/// inspect its original responsibility after a failed attempt.
pub struct Construction {
    preparation: Preparation,
}

impl Construction {
    pub const fn new() -> Self {
        Self {
            preparation: Preparation::Open,
        }
    }

    pub fn reservation(&self) -> Option<(RegionId, AllowanceId)> {
        match self.preparation {
            Preparation::Reserved(reservation) | Preparation::Closed(Some(reservation)) => {
                Some((reservation.id.region, reservation.allowance))
            }
            _ => None,
        }
    }

    pub fn close(&mut self) {
        match self.preparation {
            Preparation::Open => self.preparation = Preparation::Closed(None),
            Preparation::Reserved(reservation) => {
                self.preparation = Preparation::Closed(Some(reservation));
            }
            _ => {}
        }
    }

    /// The owner either keeps the issued material here or moves it to the
    /// actual consumer's durable allocation field while holding its protection.
    ///
    /// # Safety
    ///
    /// The original receiver must guarantee the next exact receiving position
    /// and preserve the obligation through failure and recovery.
    pub unsafe fn take(&mut self) -> Option<Allocation> {
        match &mut self.preparation {
            Preparation::Issued(allocation) => allocation.take(),
            _ => None,
        }
    }
}

impl Default for Construction {
    fn default() -> Self {
        Self::new()
    }
}

impl State {
    fn capacity(&self, count: usize) -> Result<(), Error> {
        let error = |reason, capacity| match reason {
            ReserveError::IdentityExhausted => Error::IdentityExhausted,
            _ => capacity,
        };
        self.allocations
            .check_reserve(count)
            .map_err(|e| error(e, Error::AllocationCapacity))?;
        self.returns
            .check_reserve(count)
            .map_err(|e| error(e, Error::ReturnCapacity))?;
        Ok(())
    }

    pub(super) fn record(
        &mut self,
        region: RegionId,
        physical: Physical,
        allowance: AllowanceId,
        phase: Phase,
    ) -> AllocationId {
        // Every creation preflights both exact tables under this protection.
        let allocation = self
            .allocations
            .reserve()
            .expect("Allocation slot preflighted");
        let retirement = self.returns.reserve().expect("Return slot preflighted");
        let id = AllocationId {
            region,
            key: allocation.key(),
        };
        let return_id = ReturnId {
            region,
            key: retirement.key(),
        };
        self.returns
            .publish(retirement, ReturnRecord::Reserved(id))
            .unwrap_or_else(|_| unreachable!("new Return reservation"));
        self.allocations
            .publish(
                allocation,
                AllocationRecord {
                    physical,
                    allowance,
                    retirement: return_id,
                    phase,
                },
            )
            .unwrap_or_else(|_| unreachable!("new Allocation reservation"));
        id
    }

    fn active(&self, allocation: &Allocation) -> Result<AllocationRecord, Error> {
        let record = *self
            .allocations
            .get(allocation.id.key)
            .ok_or(Error::Stale)?;
        if record.physical != allocation.physical || !matches!(record.phase, Phase::Issued) {
            return Err(Error::Stale);
        }
        Ok(record)
    }

    fn settle_reserved(&mut self, id: AllocationId, record: AllocationRecord) {
        remove(&mut self.returns, record.retirement.key);
        remove(&mut self.allocations, id.key);
    }
}

fn remove<Scope: Copy + Eq, T>(table: &mut GenerationTable<'_, Scope, T>, key: Key<Scope>) {
    table.detach(key).expect("exact published identity");
    let (reclamation, _) = table.reclaim(key).expect("exact detached identity");
    table
        .release(reclamation)
        .unwrap_or_else(|_| unreachable!("exact reclamation"));
}

fn reservation(
    source: &Allocator,
    held: &mut HeldLockSet<'_>,
    construction: &'static TurnLock<Construction>,
    closed: bool,
) -> Result<Reservation, Error> {
    let receiver = held.access(construction).ok_or(Error::Protection)?;
    let reservation = match (closed, &receiver.preparation) {
        (false, Preparation::Reserved(reservation))
        | (true, Preparation::Closed(Some(reservation))) => *reservation,
        _ => return Err(Error::Receiver),
    };
    if !ptr::eq(source, reservation.source) {
        return Err(Error::Stale);
    }
    Ok(reservation)
}

impl Allocator {
    pub fn plan_construction<'a>(
        &'a self,
        plan: &mut LockPlan<'a, '_>,
        region: RegionId,
        allowance: AllowanceId,
        construction: &'static TurnLock<Construction>,
    ) -> Result<(), Error> {
        let region = self.region(region)?;
        let allowance = self.slot(allowance)?;
        plan.insert(&region.state).map_err(|_| Error::Capacity)?;
        plan.insert(allowance).map_err(|_| Error::Capacity)?;
        plan.insert(construction).map_err(|_| Error::Capacity)?;
        Ok(())
    }

    /// # Safety
    ///
    /// The complete plan must also protect the actual authorization. The
    /// caller must revalidate its live role, ancestry, requested purpose and
    /// the exact candidate's layout and Allowance against that authorization.
    /// The construction is the real durable original request and receiver.
    pub unsafe fn reserve(
        &'static self,
        held: &mut HeldLockSet<'_>,
        candidate: Candidate,
        allowance: AllowanceId,
        construction: &'static TurnLock<Construction>,
    ) -> Result<AllocationId, Error> {
        if !ptr::eq(self, candidate.source) {
            return Err(Error::Stale);
        }
        if !matches!(
            held.access(construction)
                .ok_or(Error::Protection)?
                .preparation,
            Preparation::Open
        ) {
            return Err(Error::Receiver);
        }
        let region = self.region(candidate.region)?;
        let slot = self.slot(allowance)?;
        let amount = length(candidate.physical);
        let mut balance = held
            .access(slot)
            .ok_or(Error::Protection)?
            .open(allowance)?;
        if amount > balance.available() {
            return Err(Error::Balance);
        }
        let state = held.access(&region.state).ok_or(Error::Protection)?;
        state.capacity(1)?;
        state
            .buddy()
            .occupy(range(candidate.physical))
            .map_err(|_| Error::Changed)?;
        let id = state.record(
            candidate.region,
            candidate.physical,
            allowance,
            Phase::Reserved(construction),
        );
        balance.reserved += amount;
        held.access(slot).expect("Allowance").state = SlotState::Open(balance);
        held.access(construction).expect("receiver").preparation =
            Preparation::Reserved(Reservation {
                source: self,
                id,
                allowance,
                candidate,
            });
        Ok(id)
    }

    /// # Safety
    ///
    /// Revalidate the same actual spending authority, purpose, layout and
    /// receiving responsibility under the complete live plan. Issuance may
    /// take place only in the still open original construction.
    pub unsafe fn issue(
        &'static self,
        held: &mut HeldLockSet<'_>,
        construction: &'static TurnLock<Construction>,
    ) -> Result<AllocationId, Error> {
        let reservation = reservation(self, held, construction, false)?;
        let region = self.region(reservation.id.region)?;
        let slot = self.slot(reservation.allowance)?;
        let mut balance = held
            .access(slot)
            .ok_or(Error::Protection)?
            .open(reservation.allowance)?;
        let state = held.access(&region.state).ok_or(Error::Protection)?;
        let record = state
            .allocations
            .get_mut(reservation.id.key)
            .ok_or(Error::Stale)?;
        if !matches!(record.phase, Phase::Reserved(owner) if ptr::eq(owner, construction)) {
            return Err(Error::Stale);
        }
        assert_eq!(record.physical, reservation.candidate.physical);
        let physical = record.physical;
        record.phase = Phase::Issued;
        let amount = length(physical);
        balance.reserved -= amount;
        balance.occupied += amount;
        held.access(slot).expect("Allowance").state = SlotState::Open(balance);
        held.access(construction).expect("receiver").preparation =
            Preparation::Issued(Some(Allocation::issued(self, reservation.id, physical)));
        Ok(reservation.id)
    }

    /// Read the immutable request of the actual reserved construction under
    /// its lock for the final authorization check before Issue.
    pub fn reserved_candidate(
        &self,
        held: &mut HeldLockSet<'_>,
        construction: &'static TurnLock<Construction>,
    ) -> Result<Candidate, Error> {
        Ok(reservation(self, held, construction, false)?.candidate)
    }

    /// Cancelling a closed preparation needs no new spending authority.
    pub fn cancel(
        &self,
        held: &mut HeldLockSet<'_>,
        construction: &'static TurnLock<Construction>,
    ) -> Result<(), Error> {
        let reservation = reservation(self, held, construction, true)?;
        let region = self.region(reservation.id.region)?;
        let slot = self.slot(reservation.allowance)?;
        let (_, open) = held
            .access(slot)
            .ok_or(Error::Protection)?
            .live(reservation.allowance)?;
        let state = held.access(&region.state).ok_or(Error::Protection)?;
        let record = *state
            .allocations
            .get(reservation.id.key)
            .ok_or(Error::Stale)?;
        if !matches!(record.phase, Phase::Reserved(owner) if ptr::eq(owner, construction)) {
            return Err(Error::Stale);
        }
        let amount = length(record.physical);
        let credit = if open { 0 } else { amount };
        let unassigned = state.unassigned.checked_add(credit).ok_or(Error::Balance)?;
        state
            .buddy()
            .release(range(record.physical))
            .expect("reserved physical range");
        state.settle_reserved(reservation.id, record);
        state.unassigned = unassigned;
        let refund = held.access(slot).expect("Allowance").refund(amount, false);
        debug_assert_eq!(refund, credit);
        held.access(construction).expect("receiver").preparation = Preparation::Closed(None);
        Ok(())
    }

    pub(super) fn return_state(&self, id: ReturnId) -> Result<&TurnLock<State>, Error> {
        Ok(&self.region(id.region)?.state)
    }

    /// Move the exact retired input to its prepaid Return slot and record that
    /// identity in the original owner's durable receipt position before releasing
    /// the Region. Pending discovery can then finish even if the caller loses
    /// the success response. Rejection retains both the input and the receipt.
    pub fn begin_return(
        &self,
        retired: Retired,
        receipt: &mut Option<ReturnId>,
    ) -> Result<(), (Retired, Error)> {
        let allocation = &retired.0;
        let result = (|| {
            if receipt.is_some() {
                return Err(Error::Receiver);
            }
            if !ptr::eq(self, allocation.source) {
                return Err(Error::Stale);
            }
            let region = self.region(allocation.id.region)?;
            with_turn(&region.state, |state| {
                // The original Retired input certifies all prior uses ended.
                *receipt = Some(unsafe { state.begin_retired(allocation, ReceiptOwner::Caller) }?);
                Ok(())
            })?
        })();
        result.map_err(|error| (retired, error))
    }

    /// Finish a prepaid return once. Done can be replayed independently of
    /// the active slot and the original Allowance.
    pub fn finish_return(&self, id: ReturnId, mode: Acquisition) -> Result<Returned, Error> {
        let region = self.region(id.region)?;
        let allowance = match with_turn(&region.state, |state| state.returns.get(id.key).copied())?
        {
            Some(ReturnRecord::Pending(_, allowance, _)) => allowance,
            Some(ReturnRecord::Done(done, _)) => return Ok(done),
            _ => return Err(Error::Stale),
        };
        let slot = self.slot(allowance)?;
        let mut work = Work::<2>::new();
        let mut plan = work.plan();
        plan.insert(&region.state).expect("return Region");
        plan.insert(slot).expect("original Allowance");
        plan.execute(mode, |held| {
            let state = held.access(&region.state).expect("return Region");
            let (allocation, owner) = match state.returns.get(id.key) {
                Some(ReturnRecord::Pending(allocation, original, owner))
                    if *original == allowance =>
                {
                    (*allocation, *owner)
                }
                Some(ReturnRecord::Done(done, _)) => return Ok(*done),
                _ => return Err(Error::Stale),
            };
            let record = *state.allocations.get(allocation.key).ok_or(Error::Stale)?;
            if record.retirement != id
                || record.allowance != allowance
                || !matches!(record.phase, Phase::Returning)
            {
                return Err(Error::Stale);
            }

            let (_, open) = held
                .access(slot)
                .expect("original Allowance")
                .live(allowance)?;
            let amount = length(record.physical);
            let state = held.access(&region.state).expect("return Region");
            let credit = if open { 0 } else { amount };
            let unassigned = state.unassigned.checked_add(credit).ok_or(Error::Balance)?;
            state
                .buddy()
                .release(range(record.physical))
                .expect("original occupied range");
            state.unassigned = unassigned;
            remove(&mut state.allocations, allocation.key);
            let done = Returned {
                allocation,
                physical: record.physical,
            };
            *state.returns.get_mut(id.key).expect("original Return") =
                ReturnRecord::Done(done, owner);
            let refund = held
                .access(slot)
                .expect("original Allowance")
                .refund(amount, true);
            debug_assert_eq!(refund, credit);
            Ok(done)
        })?
    }

    /// Advance a caller-owned cyclic recovery pass through the prepaid table.
    /// A failed acquisition leaves the cursor unchanged. End resets to zero,
    /// so subsequent passes revisit Pending entries and delayed publications.
    pub fn pending(
        &self,
        id: RegionId,
        cursor: &mut usize,
        budget: usize,
    ) -> Result<Pending, Error> {
        let region = self.region(id)?;
        with_turn(&region.state, |state| {
            let end = state.returns.capacity();
            let stop = (*cursor).saturating_add(budget.min(64)).min(end);
            while *cursor < stop {
                let current = *cursor;
                *cursor += 1;
                if let Some((key, ReturnRecord::Pending(..))) = state.returns.published_at(current)
                {
                    return Pending::Found(ReturnId { region: id, key });
                }
            }
            if *cursor >= end {
                *cursor = 0;
                Pending::End
            } else {
                Pending::More
            }
        })
    }

    /// The trusted original receipt owner ends the independent history.
    /// A Pool-owned receipt can only be cleared in its joint reconciliation.
    pub fn acknowledge(&self, id: ReturnId) -> Result<(), Error> {
        with_turn(self.return_state(id)?, |state| {
            state.acknowledge_return(id, ReceiptOwner::Caller)
        })?
    }
}

impl State {
    /// The caller holds the original Region and proves all mapped, software
    /// and hardware uses of this Allocation have ended.
    pub(super) unsafe fn begin_retired(
        &mut self,
        allocation: &Allocation,
        owner: ReceiptOwner,
    ) -> Result<ReturnId, Error> {
        let record = self.active(allocation)?;
        let retirement = self
            .returns
            .get_mut(record.retirement.key)
            .ok_or(Error::Stale)?;
        if !matches!(*retirement, ReturnRecord::Reserved(id) if id == allocation.id) {
            return Err(Error::Stale);
        }
        *retirement = ReturnRecord::Pending(allocation.id, record.allowance, owner);
        self.allocations
            .get_mut(allocation.id.key)
            .expect("active")
            .phase = Phase::Returning;
        Ok(record.retirement)
    }

    pub(super) fn acknowledge_return(
        &mut self,
        id: ReturnId,
        owner: ReceiptOwner,
    ) -> Result<(), Error> {
        match self.returns.get(id.key) {
            Some(ReturnRecord::Done(_, original)) if *original == owner => {}
            Some(ReturnRecord::Done(..)) => return Err(Error::Protection),
            _ => return Err(Error::Stale),
        }
        remove(&mut self.returns, id.key);
        Ok(())
    }
}

impl Allocator {
    /// Convert one retired physical disposition into two independent ones.
    /// The original and both receiving positions stay intact on rejection.
    ///
    /// # Safety
    ///
    /// The receivers must be the actual durable positions of the new owners.
    /// The calling domain must have completed its original access and close
    /// handoff, with no additional shared role requiring this commit.
    pub unsafe fn split(
        &'static self,
        input: Retired,
        boundary: u64,
        receivers: &mut [Option<Allocation>; 2],
        mode: Acquisition,
    ) -> Result<(), (Retired, Error)> {
        let physical = input.physical();
        if boundary <= physical.start
            || boundary >= physical.end
            || !boundary.is_multiple_of(self.config.granule)
        {
            return Err((input, Error::Layout));
        }
        let output = [
            Physical {
                start: physical.start,
                end: boundary,
            },
            Physical {
                start: boundary,
                end: physical.end,
            },
        ];
        let result = self.convert([&input], output, receivers, mode);
        result.map_err(|error| (input, error))
    }

    /// Merge consecutive input dispositions without rewriting Buddy or
    /// changing their original funding. The output has its own prepaid return.
    ///
    /// # Safety
    ///
    /// The receiver must be the actual durable position of the new owner.
    /// Both calling domains must have completed the original close handoff,
    /// with no additional shared role requiring this commit.
    pub unsafe fn merge(
        &'static self,
        left: Retired,
        right: Retired,
        receiver: &mut Option<Allocation>,
        mode: Acquisition,
    ) -> Result<(), (Retired, Retired, Error)> {
        let fits =
            left.physical().end == right.physical().start && left.0.id.region == right.0.id.region;
        if !fits {
            return Err((left, right, Error::Range));
        }
        let physical = Physical {
            start: left.physical().start,
            end: right.physical().end,
        };
        let result = self.convert(
            [&left, &right],
            [physical],
            core::array::from_mut(receiver),
            mode,
        );
        result.map_err(|error| (left, right, error))
    }

    fn convert<const INPUTS: usize, const OUTPUTS: usize>(
        &'static self,
        inputs: [&Retired; INPUTS],
        outputs: [Physical; OUTPUTS],
        receivers: &mut [Option<Allocation>; OUTPUTS],
        mode: Acquisition,
    ) -> Result<(), Error> {
        if receivers.iter().any(Option::is_some) {
            return Err(Error::Receiver);
        }
        if inputs.iter().any(|input| !ptr::eq(self, input.0.source)) {
            return Err(Error::Stale);
        }
        let id = inputs[0].0.id.region;
        let region = self.region(id)?;
        let allowance = with_turn(&region.state, |state| state.active(&inputs[0].0))??.allowance;
        let slot = self.slot(allowance)?;
        let mut work = Work::<2>::new();
        let mut plan = work.plan();
        plan.insert(&region.state).expect("conversion Region");
        plan.insert(slot).expect("original Allowance");
        plan.execute(mode, |held| {
            held.access(slot).expect("Allowance").open(allowance)?;
            let state = held.access(&region.state).expect("Region");
            let mut records = [None; INPUTS];
            for (cell, input) in records.iter_mut().zip(inputs) {
                let record = state.active(&input.0)?;
                if record.allowance != allowance {
                    return Err(Error::Balance);
                }
                if !matches!(state.returns.get(record.retirement.key),
                    Some(ReturnRecord::Reserved(original)) if *original == input.0.id)
                {
                    return Err(Error::Stale);
                }
                *cell = Some(record);
            }
            state.capacity(OUTPUTS)?;
            let mut allocations = [const { None }; OUTPUTS];
            let mut returns = [const { None }; OUTPUTS];
            state
                .allocations
                .reserve_many(&mut allocations)
                .expect("preflighted output slots");
            state
                .returns
                .reserve_many(&mut returns)
                .expect("preflighted output returns");

            for ((physical, receiver), (allocation, retirement)) in outputs
                .into_iter()
                .zip(receivers)
                .zip(allocations.into_iter().zip(returns))
            {
                let allocation = allocation.expect("output allocation reservation");
                let retirement = retirement.expect("output return reservation");
                let allocation_id = AllocationId {
                    region: id,
                    key: allocation.key(),
                };
                let return_id = ReturnId {
                    region: id,
                    key: retirement.key(),
                };
                state
                    .returns
                    .publish(retirement, ReturnRecord::Reserved(allocation_id))
                    .unwrap_or_else(|_| unreachable!("new Return reservation"));
                state
                    .allocations
                    .publish(
                        allocation,
                        AllocationRecord {
                            physical,
                            allowance,
                            retirement: return_id,
                            phase: Phase::Issued,
                        },
                    )
                    .unwrap_or_else(|_| unreachable!("new Allocation reservation"));
                *receiver = Some(Allocation::issued(self, allocation_id, physical));
            }
            for (input, record) in inputs.into_iter().zip(records) {
                state.settle_reserved(input.0.id, record.expect("all inputs preflighted"));
            }
            Ok(())
        })?
    }
}
