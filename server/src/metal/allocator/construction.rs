//! Durable allocation preparation and its original receiving position.

use core::ptr;

use crate::{
    address::length,
    metal::sync::{TurnAccess, TurnLock, TurnMode, TurnPlan, TurnWorkspace},
};

use super::{
    Allocation, AllocationId, Allocator, AllowanceId, Candidate, Error, RegionId,
    allocation::Phase, allowance::SlotState,
};

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

impl Allocator {
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
    /// caller must revalidate its live role, ancestry, requested purpose and
    /// the exact candidate's layout and Allowance against that authorization.
    /// The construction is the real durable original request and receiver.
    pub(crate) unsafe fn reserve(
        &'static self,
        held: &mut TurnAccess<'_>,
        candidate: Candidate,
        allowance: AllowanceId,
        construction: &'static AllocationConstruction,
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
        construction: &'static AllocationConstruction,
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
        construction: &'static AllocationConstruction,
    ) -> Result<Candidate, Error> {
        Ok(reservation(self, held, construction)?.candidate)
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
        *held.access(&construction.state).expect("receiver") = Preparation::Shut(None);
        Ok(())
    }
}
