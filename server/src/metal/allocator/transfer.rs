//! Exact physical occupation and settlement. The Region's two tables have
//! distinct scopes and lifetimes even when a historical return names an old
//! allocation whose active slot has been reused.

use core::ptr;

use crate::{
    address::{PhysicalAddress, PhysicalRange, length},
    metal::{
        identity::{GenerationTable, Key, Reservation as SlotReservation, ReserveError},
        sync::{TurnAccess, TurnLock, TurnMode, TurnPlan, TurnWorkspace},
    },
};

use super::{
    Allocator, AllowanceId, Candidate, Error, RegionId, allowance::SlotState, region::State,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct AllocationScope(pub(super) RegionId);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ReturnScope(pub(super) RegionId);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AllocationId {
    key: Key<AllocationScope>,
}

impl AllocationId {
    fn region(self) -> RegionId {
        self.key.scope().0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReturnId {
    key: Key<ReturnScope>,
}

impl ReturnId {
    fn region(self) -> RegionId {
        self.key.scope().0
    }
}

#[must_use]
pub struct Allocation {
    source: &'static Allocator,
    id: AllocationId,
    physical: PhysicalRange,
}

/// Ownership and completed retirement of the exact original material.
#[must_use]
pub struct Retired(Allocation);

impl Allocation {
    pub(super) fn issued(
        source: &'static Allocator,
        id: AllocationId,
        physical: PhysicalRange,
    ) -> Self {
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
        self.id.region()
    }

    pub fn physical(&self) -> PhysicalRange {
        self.physical
    }

    pub(super) fn source(&self) -> &'static Allocator {
        self.source
    }

    /// The prepaid Return location belongs to the upstream source's table.
    /// Learning its identity does not authorize handoff or completion.
    pub(super) fn return_position(&self) -> Result<ReturnId, Error> {
        let region = self.source.region(self.id.region())?;
        region
            .state
            .acquire(|state| state.active(self).map(|record| record.retirement))?
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
    pub fn physical(&self) -> PhysicalRange {
        self.0.physical
    }

    /// Hand off to the original source and record the prepaid refund
    /// receipt before releasing its Region Turn.
    ///
    /// # Safety
    ///
    /// The output must be the actual owner's durable receipt position.
    pub unsafe fn begin_return(
        self,
        receipt: &mut Option<ReturnReceipt>,
    ) -> Result<(), (Self, Error)> {
        if receipt.is_some() {
            return Err((self, Error::Receiver));
        }
        let source = self.0.source;
        source.accept_return(self, ReceiptOwner::Receiver, |id| {
            *receipt = Some(ReturnReceipt { source, id });
        })
    }
}

/// The original owner's prepaid refund and acknowledgement position.
#[must_use]
pub struct ReturnReceipt {
    source: &'static Allocator,
    id: ReturnId,
}

impl ReturnReceipt {
    /// Finish the original refund or read its retained result.
    pub fn finish(&self, mode: TurnMode) -> Result<Returned, Error> {
        self.source.finish_return(self.id, mode)
    }

    /// Release the original owner's completed receipt position.
    pub fn acknowledge(&self) -> Result<(), Error> {
        self.source
            .return_state(self.id)?
            .acquire(|state| state.acknowledge_return(self.id, ReceiptOwner::Receiver))?
    }
}

#[derive(Clone, Copy)]
pub(super) enum Phase {
    Reserved(&'static Construction),
    Issued,
    Returning,
}

#[derive(Clone, Copy)]
pub(super) struct AllocationRecord {
    physical: PhysicalRange,
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
    Receiver,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Returned {
    pub allocation: AllocationId,
    pub physical: PhysicalRange,
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
    spender: Option<Spender>,
}

/// The canonical lock address of the original, permanently installed Pend.
#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) struct Spender(pub(crate) usize);

enum Preparation {
    Open,
    Reserved(Reservation),
    Closed(Option<Reservation>),
    Issued(Option<Allocation>),
}

impl Preparation {
    fn close(&mut self) -> Result<(), Error> {
        match *self {
            Self::Open => *self = Self::Closed(None),
            Self::Reserved(reservation) => *self = Self::Closed(Some(reservation)),
            Self::Closed(_) => {}
            Self::Issued(_) => return Err(Error::Receiver),
        }
        Ok(())
    }

    fn closed_reservation(&self) -> Result<Option<Reservation>, Error> {
        match self {
            Self::Closed(reservation) => Ok(*reservation),
            _ => Err(Error::Receiver),
        }
    }
}

/// An actual consumer's persistent, single-use preparation and receiving
/// position. The private Turn keeps an accepted reservation or material in
/// place until the original owner settles or hands it off.
pub struct Construction {
    state: TurnLock<Preparation>,
}

impl Construction {
    pub const fn new() -> Self {
        Self {
            state: TurnLock::new(Preparation::Open),
        }
    }

    pub(crate) fn reservation(&self) -> Result<(RegionId, AllowanceId), Error> {
        self.state.acquire(|state| match state {
            Preparation::Reserved(reservation) => {
                Ok((reservation.id.region(), reservation.allowance))
            }
            _ => Err(Error::Receiver),
        })?
    }

    /// Include this receiver in a joint operation without exposing its state.
    pub fn plan<'a>(&'a self, plan: &mut TurnPlan<'a, '_>) -> Result<(), Error> {
        plan.insert(&self.state).map_err(|_| Error::Capacity)
    }

    /// Close the original preparation to issuance.
    pub fn close(&self) -> Result<(), Error> {
        self.state.acquire(Preparation::close)?
    }

    /// Close the original preparation inside the consumer's complete plan.
    pub fn close_held(&self, held: &mut TurnAccess<'_>) -> Result<(), Error> {
        held.access(&self.state).ok_or(Error::Protection)?.close()
    }

    /// Cancel a closed preparation through its original Region and Allowance.
    /// A settled cancellation can be replayed from the same receiver.
    pub fn cancel(&'static self, mode: TurnMode) -> Result<(), Error> {
        let Some(reservation) = self.state.acquire(|state| state.closed_reservation())?? else {
            return Ok(());
        };
        let source = reservation.source;
        let mut work = TurnWorkspace::<3>::new();
        let mut plan = work.plan();
        source.plan_construction(
            &mut plan,
            reservation.id.region(),
            reservation.allowance,
            self,
        )?;
        plan.acquire(mode, |held| source.cancel(held, self))?
    }

    /// The owner either keeps the issued material here or moves it to the
    /// actual consumer's durable allocation field while holding its protection.
    ///
    /// # Safety
    ///
    /// The original receiver must guarantee the next exact receiving position
    /// and preserve the obligation through failure and recovery.
    pub unsafe fn take(&self, receiver: &mut Option<Allocation>) -> Result<(), Error> {
        if receiver.is_some() {
            return Err(Error::Receiver);
        }
        self.state.acquire(|state| {
            let Preparation::Issued(allocation) = state else {
                return Err(Error::Receiver);
            };
            *receiver = Some(allocation.take().ok_or(Error::Receiver)?);
            Ok(())
        })?
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
        physical: PhysicalRange,
        allowance: AllowanceId,
        phase: Phase,
    ) -> AllocationId {
        // Every creation preflights both exact tables under this protection.
        let allocation = self
            .allocations
            .reserve()
            .expect("Allocation slot preflighted");
        let retirement = self.returns.reserve().expect("Return slot preflighted");
        self.publish_record(allocation, retirement, physical, allowance, phase)
    }

    fn publish_record(
        &mut self,
        allocation: SlotReservation<AllocationScope>,
        retirement: SlotReservation<ReturnScope>,
        physical: PhysicalRange,
        allowance: AllowanceId,
        phase: Phase,
    ) -> AllocationId {
        let id = AllocationId {
            key: allocation.key(),
        };
        let return_id = ReturnId {
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

fn remove<Scope: Copy + Eq, T: Copy>(table: &mut GenerationTable<'_, Scope, T>, key: Key<Scope>) {
    table.detach(key).expect("exact published identity");
    let (reclamation, _) = table.reclaim(key).expect("exact detached identity");
    table
        .release(reclamation)
        .unwrap_or_else(|_| unreachable!("exact reclamation"));
}

fn reservation(
    source: &Allocator,
    held: &mut TurnAccess<'_>,
    construction: &'static Construction,
) -> Result<Reservation, Error> {
    let receiver = held.access(&construction.state).ok_or(Error::Protection)?;
    let Preparation::Reserved(reservation) = *receiver else {
        return Err(Error::Receiver);
    };
    if !ptr::eq(source, reservation.source) {
        return Err(Error::Stale);
    }
    Ok(reservation)
}

impl Allocator {
    pub fn plan_construction<'a>(
        &'a self,
        plan: &mut TurnPlan<'a, '_>,
        region: RegionId,
        allowance: AllowanceId,
        construction: &'static Construction,
    ) -> Result<(), Error> {
        let region = self.region(region)?;
        let allowance = self.slot(allowance)?;
        plan.insert(&region.state).map_err(|_| Error::Capacity)?;
        plan.insert(allowance).map_err(|_| Error::Capacity)?;
        construction.plan(plan)
    }

    /// # Safety
    ///
    /// The complete plan must also protect the actual authorization. The
    /// caller must revalidate its live role, ancestry, requested purpose and
    /// the exact candidate's layout and Allowance against that authorization.
    /// The construction is the real durable original request and receiver.
    pub(crate) unsafe fn reserve(
        &'static self,
        held: &mut TurnAccess<'_>,
        candidate: Candidate,
        allowance: AllowanceId,
        construction: &'static Construction,
        spender: Option<Spender>,
    ) -> Result<AllocationId, Error> {
        let region = self.region(candidate.region)?;
        if !matches!(
            held.access(&construction.state).ok_or(Error::Protection)?,
            Preparation::Open
        ) {
            return Err(Error::Receiver);
        }
        let slot = self.slot(allowance)?;
        let amount = length(candidate.physical).get();
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
            .occupy(candidate.physical)
            .map_err(|_| Error::Changed)?;
        let id = state.record(candidate.physical, allowance, Phase::Reserved(construction));
        balance.reserved += amount;
        held.access(slot).expect("Allowance").state = SlotState::Open(balance);
        *held.access(&construction.state).expect("receiver") = Preparation::Reserved(Reservation {
            source: self,
            id,
            allowance,
            candidate,
            spender,
        });
        Ok(id)
    }

    /// # Safety
    ///
    /// Revalidate the same actual spending authority, purpose, layout and
    /// receiving responsibility under the complete live plan. Issuance may
    /// take place only in the still open original construction.
    pub(crate) unsafe fn issue(
        &'static self,
        held: &mut TurnAccess<'_>,
        construction: &'static Construction,
        spender: Option<Spender>,
    ) -> Result<AllocationId, Error> {
        let reservation = reservation(self, held, construction)?;
        if reservation.spender != spender {
            return Err(Error::Protection);
        }
        let region = self.region(reservation.id.region())?;
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
        let amount = length(physical).get();
        balance.reserved -= amount;
        balance.occupied += amount;
        held.access(slot).expect("Allowance").state = SlotState::Open(balance);
        *held.access(&construction.state).expect("receiver") =
            Preparation::Issued(Some(Allocation::issued(self, reservation.id, physical)));
        Ok(reservation.id)
    }

    /// Read the immutable request of the actual reserved construction under
    /// its lock for the final authorization check before Issue.
    pub(crate) fn reserved_candidate(
        &self,
        held: &mut TurnAccess<'_>,
        construction: &'static Construction,
    ) -> Result<Candidate, Error> {
        Ok(reservation(self, held, construction)?.candidate)
    }

    /// Cancelling a closed preparation needs no new spending authority.
    fn cancel(
        &self,
        held: &mut TurnAccess<'_>,
        construction: &'static Construction,
    ) -> Result<(), Error> {
        let receiver = held.access(&construction.state).ok_or(Error::Protection)?;
        let Some(reservation) = receiver.closed_reservation()? else {
            return Ok(());
        };
        if !ptr::eq(self, reservation.source) {
            return Err(Error::Stale);
        }
        let region = self.region(reservation.id.region())?;
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
        let amount = length(record.physical).get();
        let credit = if open { 0 } else { amount };
        let unassigned = state.unassigned.checked_add(credit).ok_or(Error::Balance)?;
        state
            .buddy()
            .release(record.physical)
            .expect("reserved physical range");
        state.settle_reserved(reservation.id, record);
        state.unassigned = unassigned;
        let refund = held.access(slot).expect("Allowance").refund(amount, false);
        debug_assert_eq!(refund, credit);
        *held.access(&construction.state).expect("receiver") = Preparation::Closed(None);
        Ok(())
    }

    pub(super) fn return_state(&self, id: ReturnId) -> Result<&TurnLock<State>, Error> {
        Ok(&self.region(id.region())?.state)
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
        if receipt.is_some() {
            return Err((retired, Error::Receiver));
        }
        self.accept_return(retired, ReceiptOwner::Caller, |id| {
            *receipt = Some(id);
        })
    }

    fn accept_return(
        &self,
        retired: Retired,
        owner: ReceiptOwner,
        record: impl FnOnce(ReturnId),
    ) -> Result<(), (Retired, Error)> {
        let allocation = &retired.0;
        let result = (|| {
            if !ptr::eq(self, allocation.source) {
                return Err(Error::Stale);
            }
            let region = self.region(allocation.id.region())?;
            region.state.acquire(|state| {
                // The original Retired input certifies all prior uses ended.
                record(unsafe { state.begin_retired(allocation, owner) }?);
                Ok(())
            })?
        })();
        result.map_err(|error| (retired, error))
    }

    /// Finish a prepaid return once. Done can be replayed independently of
    /// the active slot and the original Allowance.
    pub fn finish_return(&self, id: ReturnId, mode: TurnMode) -> Result<Returned, Error> {
        let region = self.region(id.region())?;
        let allowance = match region
            .state
            .acquire(|state| state.returns.get(id.key).copied())?
        {
            Some(ReturnRecord::Pending(_, allowance, _)) => allowance,
            Some(ReturnRecord::Done(done, _)) => return Ok(done),
            _ => return Err(Error::Stale),
        };
        let slot = self.slot(allowance)?;
        let mut work = TurnWorkspace::<2>::new();
        let mut plan = work.plan();
        plan.insert(&region.state).expect("return Region");
        plan.insert(slot).expect("original Allowance");
        plan.acquire(mode, |held| {
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
            let amount = length(record.physical).get();
            let state = held.access(&region.state).expect("return Region");
            let credit = if open { 0 } else { amount };
            let unassigned = state.unassigned.checked_add(credit).ok_or(Error::Balance)?;
            state
                .buddy()
                .release(record.physical)
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
        Ok(region.state.acquire(|state| {
            let end = state.returns.capacity();
            let stop = (*cursor).saturating_add(budget.min(64)).min(end);
            while *cursor < stop {
                let current = *cursor;
                *cursor += 1;
                if let Some((key, ReturnRecord::Pending(..))) = state.returns.published_at(current)
                {
                    return Pending::Found(ReturnId { key });
                }
            }
            if *cursor >= end {
                *cursor = 0;
                Pending::End
            } else {
                Pending::More
            }
        })?)
    }

    /// The trusted original receipt owner ends the independent history.
    /// A bound receipt retains acknowledgement authority at its original owner.
    pub fn acknowledge(&self, id: ReturnId) -> Result<(), Error> {
        self.return_state(id)?
            .acquire(|state| state.acknowledge_return(id, ReceiptOwner::Caller))?
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
        boundary: PhysicalAddress,
        receivers: &mut [Option<Allocation>; 2],
        mode: TurnMode,
    ) -> Result<(), (Retired, Error)> {
        if !boundary.is_aligned(self.config.granule_alignment()) {
            return Err((input, Error::Layout));
        }
        let Some((left, right)) = input.physical().split(boundary) else {
            return Err((input, Error::Layout));
        };
        let result = self.convert([&input], [left, right], receivers, mode);
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
        mode: TurnMode,
    ) -> Result<(), (Retired, Retired, Error)> {
        let fits = left.physical().end() == right.physical().start()
            && left.0.id.region() == right.0.id.region();
        if !fits {
            return Err((left, right, Error::Range));
        }
        let physical = left
            .physical()
            .merge(right.physical())
            .expect("consecutive allocation ranges");
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
        outputs: [PhysicalRange; OUTPUTS],
        receivers: &mut [Option<Allocation>; OUTPUTS],
        mode: TurnMode,
    ) -> Result<(), Error> {
        if receivers.iter().any(Option::is_some) {
            return Err(Error::Receiver);
        }
        if inputs.iter().any(|input| !ptr::eq(self, input.0.source)) {
            return Err(Error::Stale);
        }
        let region = self.region(inputs[0].0.id.region())?;
        let allowance = region
            .state
            .acquire(|state| state.active(&inputs[0].0))??
            .allowance;
        let slot = self.slot(allowance)?;
        let mut work = TurnWorkspace::<2>::new();
        let mut plan = work.plan();
        plan.insert(&region.state).expect("conversion Region");
        plan.insert(slot).expect("original Allowance");
        plan.acquire(mode, |held| {
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
                let allocation_id = state.publish_record(
                    allocation,
                    retirement,
                    physical,
                    allowance,
                    Phase::Issued,
                );
                *receiver = Some(Allocation::issued(self, allocation_id, physical));
            }
            for (input, record) in inputs.into_iter().zip(records) {
                state.settle_reserved(input.0.id, record.expect("all inputs preflighted"));
            }
            Ok(())
        })?
    }
}
