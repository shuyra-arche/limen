use core::{ptr, sync::atomic::Ordering::Acquire};

use crate::{
    address::length,
    sync::{Acquisition, HeldLockSet, LockPlan, TurnLock, Work},
};

use super::{Allocator, Error, REGION_LIMIT, RegionId, with_turn};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AllowanceId {
    pub(super) source: usize,
    pub(super) index: usize,
    pub(super) generation: u32,
}

/// Actual Allowance close responsibility, issued once to its receiver.
#[must_use]
pub struct Allowance {
    source: &'static Allocator,
    id: AllowanceId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Balance {
    pub capacity: u64,
    pub reserved: u64,
    pub occupied: u64,
}

impl Balance {
    pub fn available(self) -> u64 {
        self.capacity - self.reserved - self.occupied
    }
}

pub(super) struct Free {
    pub(super) head: Option<usize>,
    pub(super) retired: usize,
}

pub(super) struct Slot {
    pub(super) generation: u32,
    pub(super) state: SlotState,
}

pub(super) enum SlotState {
    Free(Option<usize>),
    Open(Balance),
    Shutting { reserved: u64, occupied: u64 },
    Retired,
}

impl Slot {
    pub(super) fn live(&self, id: AllowanceId) -> Result<(Balance, bool), Error> {
        if self.generation != id.generation {
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

    pub(crate) fn plan_spender<'a>(&self, plan: &mut LockPlan<'a, '_>) -> Result<(), Error> {
        plan.insert(self.source.slot(self.id)?)
            .map_err(|_| Error::Capacity)
    }

    pub(crate) fn check_spender(&self, held: &mut HeldLockSet<'_>) -> Result<(), Error> {
        held.access(self.source.slot(self.id)?)
            .ok_or(Error::Protection)?
            .open(self.id)?;
        Ok(())
    }

    pub fn balance(&self) -> Result<Balance, Error> {
        with_turn(self.source.slot(self.id)?, |slot| {
            slot.live(self.id).map(|pair| pair.0)
        })?
    }

    /// Close spending and immediately refund the currently unused amount to
    /// a published Region. Existing records still refund to their own Regions.
    pub fn shut(&self, credit: Option<RegionId>, mode: Acquisition) -> Result<(), Error> {
        let source = self.source;
        let slot = source.slot(self.id)?;
        let region = credit.map(|id| source.region(id)).transpose()?;
        let mut work = Work::<2>::new();
        let mut plan = work.plan();
        plan.insert(slot).expect("Allowance");
        if let Some(region) = region {
            plan.insert(&region.state).expect("credit Region");
        }
        plan.execute(mode, |held| {
            let state = held.access(slot).expect("Allowance");
            let (balance, open) = state.live(self.id)?;
            if !open {
                return Ok(());
            }
            let amount = balance.available();
            if amount != 0 {
                let region = region.ok_or(Error::Balance)?;
                let state = held.access(&region.state).expect("credit Region");
                state.unassigned = state.unassigned.checked_add(amount).ok_or(Error::Balance)?;
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
    pub fn reclaim(self, mode: Acquisition) -> Result<(), (Self, Error)> {
        let source = self.source;
        let id = self.id;
        let result = source.finish_shut(id, mode);
        result.map_err(|error| (self, error))
    }
}

impl Allocator {
    pub(super) fn slot(&self, id: AllowanceId) -> Result<&TurnLock<Slot>, Error> {
        if ptr::from_ref(self).addr() != id.source {
            return Err(Error::Stale);
        }
        self.allowances.get(id.index).ok_or(Error::Stale)
    }

    /// Coherent managed RAM and unassigned budget. The trusted kernel's
    /// observation of the exact source uses the complete Region set.
    pub fn budget(&self, mode: Acquisition) -> Result<(u64, u64), Error> {
        let count = with_turn(&self.directory, |_| self.published.load(Acquire))?;
        let mut work = Work::<{ REGION_LIMIT + 1 }>::new();
        let mut plan = work.plan();
        plan.insert(&self.directory).expect("directory");
        for region in &self.regions[..count] {
            plan.insert(&region.state).expect("published Region");
        }
        plan.execute(mode, |held| {
            if count != self.published.load(Acquire) {
                return Err(Error::Changed);
            }
            self.regions[..count]
                .iter()
                .try_fold((0u64, 0u64), |(total, unassigned), region| {
                    let state = held.access(&region.state).expect("published Region");
                    Ok((
                        total
                            .checked_add(length(region.description().physical).get())
                            .ok_or(Error::Balance)?,
                        unassigned
                            .checked_add(state.unassigned)
                            .ok_or(Error::Balance)?,
                    ))
                })
        })?
    }

    /// Observe the free-chain head before constructing the complete plan.
    pub fn allowance_candidate(&self) -> Result<usize, Error> {
        with_turn(&self.free, |free| {
            free.head.ok_or(if free.retired == self.allowances.len() {
                Error::IdentityExhausted
            } else {
                Error::AllowanceCapacity
            })
        })?
    }

    pub fn plan_allowance<'a>(
        &'a self,
        plan: &mut LockPlan<'a, '_>,
        index: usize,
        debits: &[(RegionId, u64)],
    ) -> Result<(), Error> {
        if debits.len() > self.config.regions {
            return Err(Error::Capacity);
        }
        let slot = self.allowances.get(index).ok_or(Error::Stale)?;
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
    /// at the supplied receiver. The index is only a transient candidate.
    pub unsafe fn open_allowance(
        &'static self,
        held: &mut HeldLockSet<'_>,
        index: usize,
        debits: &[(RegionId, u64)],
        receiver: &mut Option<Allowance>,
    ) -> Result<(), Error> {
        if receiver.is_some() {
            return Err(Error::Receiver);
        }
        if debits.len() > self.config.regions {
            return Err(Error::Capacity);
        }
        let slot = self.allowances.get(index).ok_or(Error::Stale)?;
        if held.access(&self.free).ok_or(Error::Protection)?.head != Some(index) {
            return Err(Error::Changed);
        }
        let entry = held.access(slot).ok_or(Error::Protection)?;
        let SlotState::Free(next) = entry.state else {
            return Err(Error::Changed);
        };
        let generation = entry
            .generation
            .checked_add(1)
            .ok_or(Error::IdentityExhausted)?;
        let mut capacity = 0u64;
        for (position, &(id, amount)) in debits.iter().enumerate() {
            if !amount.is_multiple_of(self.config.granule.get())
                || debits[..position].iter().any(|&(prior, _)| prior == id)
            {
                return Err(Error::Layout);
            }
            let state = held
                .access(&self.region(id)?.state)
                .ok_or(Error::Protection)?;
            if amount > state.unassigned {
                return Err(Error::Balance);
            }
            capacity = capacity.checked_add(amount).ok_or(Error::Balance)?;
        }
        for &(id, amount) in debits {
            let state = held
                .access(&self.regions[id.0].state)
                .expect("checked debit");
            state.unassigned -= amount;
        }
        held.access(&self.free).expect("free index").head = next;
        *held.access(slot).expect("Allowance") = Slot {
            generation,
            state: SlotState::Open(Balance {
                capacity,
                reserved: 0,
                occupied: 0,
            }),
        };
        *receiver = Some(Allowance::initial(
            self,
            AllowanceId {
                source: ptr::from_ref(self).addr(),
                index,
                generation,
            },
        ));
        Ok(())
    }

    pub fn plan_transfer<'a>(
        &'a self,
        plan: &mut LockPlan<'a, '_>,
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
        held: &mut HeldLockSet<'_>,
        from: AllowanceId,
        to: AllowanceId,
        amount: u64,
    ) -> Result<(), Error> {
        if from == to || !amount.is_multiple_of(self.config.granule.get()) {
            return Err(Error::Layout);
        }
        let sender = self.slot(from)?;
        let receiver = self.slot(to)?;
        let mut debit = held.access(sender).ok_or(Error::Protection)?.open(from)?;
        let mut credit = held.access(receiver).ok_or(Error::Protection)?.open(to)?;
        if amount > debit.available() {
            return Err(Error::Balance);
        }
        credit.capacity = credit.capacity.checked_add(amount).ok_or(Error::Balance)?;
        debit.capacity -= amount;
        held.access(sender).expect("debit").state = SlotState::Open(debit);
        held.access(receiver).expect("credit").state = SlotState::Open(credit);
        Ok(())
    }

    fn finish_shut(&self, id: AllowanceId, mode: Acquisition) -> Result<(), Error> {
        let slot = self.slot(id)?;
        let mut work = Work::<2>::new();
        let mut plan = work.plan();
        plan.insert(&self.free).expect("free index");
        plan.insert(slot).expect("Allowance");
        plan.execute(mode, |held| {
            let entry = held.access(slot).expect("Allowance");
            let (balance, open) = entry.live(id)?;
            if open || balance.capacity != 0 {
                return Err(Error::Balance);
            }
            if entry.generation == u32::MAX {
                entry.state = SlotState::Retired;
                held.access(&self.free).expect("free index").retired += 1;
            } else {
                let free = held.access(&self.free).expect("free index");
                let next = free.head.replace(id.index);
                held.access(slot).expect("Allowance").state = SlotState::Free(next);
            }
            Ok(())
        })?
    }
}
