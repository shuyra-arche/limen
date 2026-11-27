mod budget;
pub(super) mod permit;

pub use budget::{Budget, BudgetObservation, BudgetProgress};

use limen_treaty::model::ByteLength;

use crate::metal::{
    identity::{Generation, Key, Slot},
    sync::{TurnAccess, TurnLock, TurnMode, TurnPlan, TurnWorkspace},
};

use super::{Allocator, Error, RegionId, SourceId};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AllowanceScope(SourceId);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AllowanceId(Key<AllowanceScope>);

impl AllowanceId {
    pub(super) fn new(source: SourceId, slot: Slot, generation: Generation) -> Self {
        Self(Key::new(AllowanceScope(source), slot, generation))
    }
}

/// An observation of the original source's current free-chain head.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AllowanceCandidate {
    source: SourceId,
    slot: Slot,
}

/// Actual Allowance close responsibility, issued once to its receiver.
#[must_use]
pub struct Allowance {
    source: &'static Allocator,
    id: AllowanceId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Balance {
    capacity: u64,
    reserved: u64,
    occupied: u64,
}

impl Balance {
    /// A new balance starts with issued material and no pending reservation.
    pub(super) fn new(capacity: u64, occupied: u64) -> Self {
        assert!(occupied <= capacity);
        Self {
            capacity,
            reserved: 0,
            occupied,
        }
    }

    pub fn capacity(self) -> ByteLength {
        ByteLength::new(self.capacity)
    }

    pub fn reserved(self) -> ByteLength {
        ByteLength::new(self.reserved)
    }

    pub fn occupied(self) -> ByteLength {
        ByteLength::new(self.occupied)
    }

    pub fn available(self) -> ByteLength {
        ByteLength::new(self.capacity - self.reserved - self.occupied)
    }

    pub(super) fn reserve(&mut self, amount: u64) {
        assert!(amount <= self.available().get());
        self.reserved += amount;
    }

    pub(super) fn issue(&mut self, amount: u64) {
        assert!(amount <= self.reserved);
        self.reserved -= amount;
        self.occupied += amount;
    }
}

pub(super) struct Free {
    pub(super) head: Option<Slot>,
    pub(super) retired: usize,
}

pub(super) struct AllowanceSlot {
    generation: Option<Generation>,
    pub(super) state: SlotState,
}

pub(super) enum SlotState {
    Free(Option<Slot>),
    Open(Balance),
    Shutting { reserved: u64, occupied: u64 },
    Retired,
}

impl AllowanceSlot {
    pub(super) fn initial(balance: Balance) -> Self {
        Self::open_generation(Generation::FIRST, balance)
    }

    pub(super) fn free(next: Option<Slot>) -> Self {
        Self {
            generation: None,
            state: SlotState::Free(next),
        }
    }

    fn open_generation(generation: Generation, balance: Balance) -> Self {
        Self {
            generation: Some(generation),
            state: SlotState::Open(balance),
        }
    }

    pub(super) fn live(&self, id: AllowanceId) -> Result<(Balance, bool), Error> {
        if self.generation != Some(id.0.generation()) {
            return Err(Error::Stale);
        }
        match self.state {
            SlotState::Open(balance) => Ok((balance, true)),
            SlotState::Shutting { reserved, occupied } => Ok((
                Balance {
                    capacity: reserved + occupied,
                    reserved,
                    occupied,
                },
                false,
            )),
            _ => Err(Error::Stale),
        }
    }

    pub(super) fn open(&self, id: AllowanceId) -> Result<Balance, Error> {
        let (balance, open) = self.live(id)?;
        if !open {
            return Err(Error::Shut);
        }
        Ok(balance)
    }

    pub(super) fn refund(&mut self, amount: u64, issued: bool) -> u64 {
        match &mut self.state {
            SlotState::Open(balance) => {
                let debit = if issued {
                    &mut balance.occupied
                } else {
                    &mut balance.reserved
                };
                *debit -= amount;
                0
            }
            SlotState::Shutting { reserved, occupied } => {
                let debit = if issued { occupied } else { reserved };
                *debit -= amount;
                amount
            }
            _ => unreachable!("original Allowance remains live through refund"),
        }
    }
}

impl Allowance {
    pub(super) fn initial(source: &'static Allocator, id: AllowanceId) -> Self {
        Self { source, id }
    }

    pub fn source(&self) -> &'static Allocator {
        self.source
    }

    pub fn id(&self) -> AllowanceId {
        self.id
    }

    pub fn balance(&self) -> Result<Balance, Error> {
        self.source
            .slot(self.id)?
            .acquire(|slot| slot.live(self.id).map(|pair| pair.0))?
    }

    /// Close spending and immediately refund the currently unused amount to
    /// a published Region. Existing records still refund to their own Regions.
    pub fn shut(&self, credit: Option<RegionId>, mode: TurnMode) -> Result<(), Error> {
        let source = self.source;
        let slot = source.slot(self.id)?;
        let region = credit.map(|id| source.region(id)).transpose()?;
        let mut work = TurnWorkspace::<2>::new();
        let mut plan = work.plan();
        plan.insert(slot).expect("Allowance");
        if let Some(region) = region {
            plan.insert(&region.state).expect("credit Region");
        }
        plan.acquire(mode, |held| {
            let state = held.access(slot).expect("Allowance");
            let (balance, open) = state.live(self.id)?;
            if !open {
                return Ok(());
            }
            let amount = balance.available().get();
            if amount != 0 {
                let region = region.ok_or(Error::Balance)?;
                let state = held.access(&region.state).expect("credit Region");
                let next = state
                    .unassigned()
                    .checked_add(amount)
                    .ok_or(Error::Balance)?;
                state.set_unassigned(next);
            }
            held.access(slot).expect("Allowance").state = SlotState::Shutting {
                reserved: balance.reserved,
                occupied: balance.occupied,
            };
            Ok(())
        })?
    }

    /// Release an exact closed slot after its final original refund.
    /// Failure leaves the close responsibility with its owner.
    pub fn reclaim(self, mode: TurnMode) -> Result<(), (Self, Error)> {
        let source = self.source;
        let id = self.id;
        let result = source.finish_shut(id, mode);
        result.map_err(|error| (self, error))
    }
}

impl Allocator {
    pub(super) fn slot(&self, id: AllowanceId) -> Result<&TurnLock<AllowanceSlot>, Error> {
        self.allowance_slot(id.0.scope().0, id.0.slot())
    }

    fn allowance_slot(
        &self,
        source: SourceId,
        slot: Slot,
    ) -> Result<&TurnLock<AllowanceSlot>, Error> {
        if source != self.source_id() {
            return Err(Error::Stale);
        }
        self.allowances.get(slot.index()).ok_or(Error::Stale)
    }

    /// Observe the free-chain head before constructing the complete plan.
    pub fn allowance_candidate(&self) -> Result<AllowanceCandidate, Error> {
        let slot = self.free.acquire(|free| {
            free.head.ok_or(if free.retired == self.allowances.len() {
                Error::IdentityExhausted
            } else {
                Error::AllowanceCapacity
            })
        })??;
        Ok(AllowanceCandidate {
            source: self.source_id(),
            slot,
        })
    }

    pub fn plan_allowance<'a>(
        &'a self,
        plan: &mut TurnPlan<'a, '_>,
        candidate: AllowanceCandidate,
        debits: &[(RegionId, ByteLength)],
    ) -> Result<(), Error> {
        if debits.len() > self.config.regions {
            return Err(Error::Capacity);
        }
        let slot = self.allowance_slot(candidate.source, candidate.slot)?;
        plan.insert(&self.free).map_err(|_| Error::Capacity)?;
        plan.insert(slot).map_err(|_| Error::Capacity)?;
        for &(region, _) in debits {
            plan.insert(&self.region(region)?.state)
                .map_err(|_| Error::Capacity)?;
        }
        Ok(())
    }

    /// Commit source-to-Allowance assignment with a real receiver under the
    /// complete plan, including the caller's management and first handoff roles.
    ///
    /// # Safety
    ///
    /// The caller holds and has revalidated the actual live assignment and
    /// first-publication authority and guarantees durable close responsibility
    /// at the supplied receiver. The candidate only observes the free chain.
    pub unsafe fn open_allowance(
        &'static self,
        held: &mut TurnAccess<'_>,
        candidate: AllowanceCandidate,
        debits: &[(RegionId, ByteLength)],
        receiver: &mut Option<Allowance>,
    ) -> Result<(), Error> {
        if receiver.is_some() {
            return Err(Error::Receiver);
        }
        if debits.len() > self.config.regions {
            return Err(Error::Capacity);
        }
        let slot = self.allowance_slot(candidate.source, candidate.slot)?;
        if held.access(&self.free).ok_or(Error::Protection)?.head != Some(candidate.slot) {
            return Err(Error::Changed);
        }
        let entry = held.access(slot).ok_or(Error::Protection)?;
        let SlotState::Free(next) = entry.state else {
            return Err(Error::Changed);
        };
        let generation = Generation::next(entry.generation).ok_or(Error::IdentityExhausted)?;
        let mut capacity = 0u64;
        for (position, &(id, amount)) in debits.iter().enumerate() {
            let amount = amount.get();
            if !amount.is_multiple_of(self.config.granule.get())
                || debits[..position].iter().any(|&(prior, _)| prior == id)
            {
                return Err(Error::Layout);
            }
            let state = held
                .access(&self.region(id)?.state)
                .ok_or(Error::Protection)?;
            if amount > state.unassigned() {
                return Err(Error::Balance);
            }
            capacity = capacity.checked_add(amount).ok_or(Error::Balance)?;
        }
        for &(id, amount) in debits {
            let state = held
                .access(&self.regions[id.index()].state)
                .expect("checked debit");
            state.set_unassigned(state.unassigned() - amount.get());
        }
        held.access(&self.free).expect("free index").head = next;
        *held.access(slot).expect("Allowance") =
            AllowanceSlot::open_generation(generation, Balance::new(capacity, 0));
        *receiver = Some(Allowance::initial(
            self,
            AllowanceId::new(candidate.source, candidate.slot, generation),
        ));
        Ok(())
    }

    pub fn plan_transfer<'a>(
        &'a self,
        plan: &mut TurnPlan<'a, '_>,
        from: AllowanceId,
        to: AllowanceId,
    ) -> Result<(), Error> {
        plan.insert(self.slot(from)?).map_err(|_| Error::Capacity)?;
        plan.insert(self.slot(to)?).map_err(|_| Error::Capacity)?;
        Ok(())
    }

    /// # Safety
    ///
    /// The complete plan must cover the actual debit and credit authorities.
    /// The caller must revalidate both roles and the intended amount before
    /// transferring unused capacity between two live Allowances.
    pub unsafe fn transfer_allowance(
        &self,
        held: &mut TurnAccess<'_>,
        from: AllowanceId,
        to: AllowanceId,
        amount: ByteLength,
    ) -> Result<(), Error> {
        let amount = amount.get();
        if from == to || !amount.is_multiple_of(self.config.granule.get()) {
            return Err(Error::Layout);
        }
        let sender = self.slot(from)?;
        let receiver = self.slot(to)?;
        let mut debit = held.access(sender).ok_or(Error::Protection)?.open(from)?;
        let mut credit = held.access(receiver).ok_or(Error::Protection)?.open(to)?;
        if amount > debit.available().get() {
            return Err(Error::Balance);
        }
        credit.capacity = credit.capacity.checked_add(amount).ok_or(Error::Balance)?;
        debit.capacity -= amount;
        held.access(sender).expect("debit").state = SlotState::Open(debit);
        held.access(receiver).expect("credit").state = SlotState::Open(credit);
        Ok(())
    }

    fn finish_shut(&self, id: AllowanceId, mode: TurnMode) -> Result<(), Error> {
        let slot = self.slot(id)?;
        let mut work = TurnWorkspace::<2>::new();
        let mut plan = work.plan();
        plan.insert(&self.free).expect("free index");
        plan.insert(slot).expect("Allowance");
        plan.acquire(mode, |held| {
            let entry = held.access(slot).expect("Allowance");
            let (balance, open) = entry.live(id)?;
            if open || balance.capacity != 0 {
                return Err(Error::Balance);
            }
            if id.0.generation().exhausted() {
                entry.state = SlotState::Retired;
                held.access(&self.free).expect("free index").retired += 1;
            } else {
                let free = held.access(&self.free).expect("free index");
                let next = free.head.replace(id.0.slot());
                held.access(slot).expect("Allowance").state = SlotState::Free(next);
            }
            Ok(())
        })?
    }
}
