//! Durable allocation preparation and its original receiving position.

use core::ptr;

use crate::metal::sync::{TurnAccess, TurnLock, TurnMode, TurnPlan, TurnWorkspace};

use super::super::{
    Allocation, AllocationId, Allocator, AllowanceId, Candidate, Error, RegionId,
    allocation::Phase,
    allowance::SlotState,
    allowance::permit::{AllocationPermit, AllocationPermitSlot},
};

#[derive(Clone, Copy)]
struct Reservation {
    source: &'static Allocator,
    id: AllocationId,
    allowance: AllowanceId,
    candidate: Candidate,
    permit_slot: &'static AllocationPermitSlot,
}

enum Preparation {
    Open,
    Reserved(Reservation),
    Shut(Option<Reservation>),
    Issued(Option<Allocation>),
}

impl Preparation {
    fn shut(&mut self) -> Result<(), Error> {
        match *self {
            Self::Open => *self = Self::Shut(None),
            Self::Reserved(reservation) => *self = Self::Shut(Some(reservation)),
            Self::Shut(_) => {}
            Self::Issued(_) => return Err(Error::Receiver),
        }
        Ok(())
    }

    fn shut_reservation(&self) -> Result<Option<Reservation>, Error> {
        match self {
            Self::Shut(reservation) => Ok(*reservation),
            _ => Err(Error::Receiver),
        }
    }
}

/// An actual consumer's persistent, single-use preparation and receiving
/// position. The private Turn keeps an accepted reservation or material in
/// place until the original owner settles or hands it off.
pub struct AllocationConstruction {
    state: TurnLock<Preparation>,
}

impl AllocationConstruction {
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
    pub fn shut(&self) -> Result<(), Error> {
        self.state.acquire(Preparation::shut)?
    }

    /// Close the original preparation inside the consumer's complete plan.
    pub fn shut_held(&self, held: &mut TurnAccess<'_>) -> Result<(), Error> {
        held.access(&self.state).ok_or(Error::Protection)?.shut()
    }

    /// Cancel a closed preparation through its original Region and Allowance.
    /// A settled cancellation can be replayed from the same receiver.
    pub fn cancel(&'static self, mode: TurnMode) -> Result<(), Error> {
        let Some(reservation) = self.state.acquire(|state| state.shut_reservation())?? else {
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

impl Default for AllocationConstruction {
    fn default() -> Self {
        Self::new()
    }
}

fn reservation(
    source: &Allocator,
    held: &mut TurnAccess<'_>,
    construction: &'static AllocationConstruction,
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

/// Result of inspecting one allocation position for a closed reservation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Cancellation {
    More,
    Cancelled(AllocationId),
    End,
}

impl Allocator {
    /// Inspect one position in the original Region. A failed lock attempt
    /// leaves the cursor unchanged. An end-of-pass resets it for later closes.
    pub fn cancel_closed(
        &self,
        region: RegionId,
        cursor: &mut usize,
        mode: TurnMode,
    ) -> Result<Cancellation, Error> {
        let region = self.region(region)?;
        let (capacity, candidate) = region.state.acquire(|state| {
            let table = &state.allocations;
            let candidate = table.published_at(*cursor).and_then(|(key, record)| {
                if let Phase::Reserved(construction) = record.phase {
                    Some((AllocationId { key }, record.allowance, construction))
                } else {
                    None
                }
            });
            (table.capacity(), candidate)
        })?;
        if *cursor >= capacity {
            *cursor = 0;
            return Ok(Cancellation::End);
        }
        let result = if let Some((id, allowance, construction)) = candidate {
            let mut work = TurnWorkspace::<3>::new();
            let mut plan = work.plan();
            self.plan_construction(&mut plan, id.region(), allowance, construction)?;
            plan.acquire(mode, |held| -> Result<_, Error> {
                let receiver = held
                    .access(&construction.state)
                    .expect("original construction");
                if matches!(receiver, Preparation::Shut(Some(reservation)) if reservation.id == id)
                {
                    self.cancel(held, construction)?;
                    Ok(Cancellation::Cancelled(id))
                } else {
                    Ok(Cancellation::More)
                }
            })??
        } else {
            Cancellation::More
        };
        *cursor += 1;
        Ok(result)
    }

    pub fn plan_construction<'a>(
        &'a self,
        plan: &mut TurnPlan<'a, '_>,
        region: RegionId,
        allowance: AllowanceId,
        construction: &'static AllocationConstruction,
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
    /// caller must obtain the permit from its live original holding and
    /// revalidate the branch under the same protection.
    /// The construction is the real durable original request and receiver.
    pub(crate) unsafe fn reserve(
        &'static self,
        held: &mut TurnAccess<'_>,
        candidate: Candidate,
        permit: AllocationPermit,
        construction: &'static AllocationConstruction,
    ) -> Result<AllocationId, Error> {
        let region = self.region(candidate.region)?;
        permit.check(self, candidate)?;
        let allowance = permit.allowance();
        if !matches!(
            held.access(&construction.state).ok_or(Error::Protection)?,
            Preparation::Open
        ) {
            return Err(Error::Receiver);
        }
        let slot = self.slot(allowance)?;
        let amount = candidate.physical.length().get();
        let mut balance = held
            .access(slot)
            .ok_or(Error::Protection)?
            .open(allowance)?;
        if amount > balance.available().get() {
            return Err(Error::Balance);
        }
        let state = held.access(&region.state).ok_or(Error::Protection)?;
        state.capacity(1)?;
        state
            .buddy()
            .occupy(candidate.physical)
            .map_err(|_| Error::Changed)?;
        let id = state.record(candidate.physical, allowance, Phase::Reserved(construction));
        balance.reserve(amount);
        held.access(slot).expect("Allowance").state = SlotState::Open(balance);
        *held.access(&construction.state).expect("receiver") = Preparation::Reserved(Reservation {
            source: self,
            id,
            allowance,
            candidate,
            permit_slot: permit.slot(),
        });
        Ok(id)
    }

    /// # Safety
    ///
    /// Recover the permit from its live holding under the complete plan and
    /// keep that protection through the commit. Issuance may take place only
    /// in the still open original construction.
    pub(crate) unsafe fn issue(
        &'static self,
        held: &mut TurnAccess<'_>,
        construction: &'static AllocationConstruction,
        permit: AllocationPermit,
    ) -> Result<AllocationId, Error> {
        let reservation = reservation(self, held, construction)?;
        if reservation.allowance != permit.allowance()
            || !reservation.permit_slot.matches(permit.slot())
        {
            return Err(Error::PermitOrigin);
        }
        permit.check(self, reservation.candidate)?;
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
        let amount = physical.length().get();
        balance.issue(amount);
        held.access(slot).expect("Allowance").state = SlotState::Open(balance);
        *held.access(&construction.state).expect("receiver") =
            Preparation::Issued(Some(Allocation::issued(self, reservation.id, physical)));
        Ok(reservation.id)
    }

    /// Cancelling a closed preparation needs no new spending authority.
    fn cancel(
        &self,
        held: &mut TurnAccess<'_>,
        construction: &'static AllocationConstruction,
    ) -> Result<(), Error> {
        let receiver = held.access(&construction.state).ok_or(Error::Protection)?;
        let Some(reservation) = receiver.shut_reservation()? else {
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
        let amount = record.physical.length().get();
        let credit = if open { 0 } else { amount };
        let unassigned = state
            .unassigned()
            .checked_add(credit)
            .ok_or(Error::Balance)?;
        state
            .buddy()
            .release(record.physical)
            .expect("reserved physical range");
        state.settle_reserved(reservation.id, record);
        state.set_unassigned(unassigned);
        let refund = held.access(slot).expect("Allowance").refund(amount, false);
        debug_assert_eq!(refund, credit);
        *held.access(&construction.state).expect("receiver") = Preparation::Shut(None);
        Ok(())
    }
}
