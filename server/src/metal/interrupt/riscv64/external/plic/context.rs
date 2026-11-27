//! Independent supervisor contexts, their threshold and original claim.

use super::{
    Access, Domain, Failure,
    registers::{claim, enable, enables, threshold},
};
use crate::metal::{
    interrupt::{
        riscv64::internal::supervisor::{Cause, Reception},
        treaty::{ClaimConfirmation, ClaimId, Completion, HardwareClaim, Receive, claim::Sealed},
    },
    platform::description::devicetree::{DeviceTree, Error},
    processor::description::riscv64::cpu::RiscvCpus,
};

/// One supervisor context described by the physical controller.
#[derive(Clone, Copy, Debug)]
pub struct Destination<'a> {
    pub(super) domain: Domain<'a>,
    pub(super) hart: u64,
    pub(super) index: usize,
}

impl<'a> Destination<'a> {
    pub fn read(
        tree: &DeviceTree<'a>,
        cpus: &RiscvCpus<'a>,
        domain: Domain<'a>,
        hart: u64,
    ) -> Result<Self, Error> {
        let node = tree.node(domain.node).ok_or(Error::Conflict)?;
        if tree.node(cpus.node().id()).is_none() {
            return Err(Error::Conflict);
        }
        if !cpus.cpu(hart)?.is_some_and(|cpu| cpu.enabled()) {
            return Err(Error::Missing);
        }
        let bytes = node
            .property(b"interrupts-extended")?
            .ok_or(Error::Missing)?;
        // Domain checked the shape, length and register extent in this tree.
        let mut found = None;
        for (index, pair) in bytes.as_chunks::<8>().0.iter().enumerate() {
            let cause = u32::from_be_bytes(pair[4..].try_into().unwrap());
            // Machine and firmware-disabled destinations still occupy indexes.
            if matches!(cause, 11 | u32::MAX) {
                continue;
            }
            if cause != 9 {
                return Err(Error::Unsupported);
            }
            let handle = u32::from_be_bytes(pair[..4].try_into().unwrap());
            if cpus.interrupt_hart(handle)? == hart && found.replace(index).is_some() {
                return Err(Error::Conflict);
            }
        }
        Ok(Self {
            domain,
            hart,
            index: found.ok_or(Error::Missing)?,
        })
    }

    pub fn number(self) -> usize {
        self.index
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum State {
    Prepared,
    Opening,
    Open,
    Receiving(u64),
    Received(u64, u32),
    Completing(u64, u32),
    Awaiting(u64, u32),
    Shutting,
    Shut,
}

impl State {
    fn reborrowable_claim(self) -> Result<Option<(u64, u32)>, Failure> {
        match self {
            Self::Receiving(_) | Self::Completing(..) => Err(Failure::Unconfirmed),
            Self::Received(round, source) | Self::Awaiting(round, source) => {
                Ok(Some((round, source)))
            }
            Self::Prepared | Self::Opening | Self::Open | Self::Shutting | Self::Shut => Ok(None),
        }
    }

    pub(super) fn retains_claim(self, source: u32) -> bool {
        match self {
            Self::Receiving(_) => true,
            Self::Received(_, claimed)
            | Self::Completing(_, claimed)
            | Self::Awaiting(_, claimed) => claimed == source,
            _ => false,
        }
    }

    pub(super) fn excludes_source(self, source: u32) -> bool {
        match self {
            Self::Prepared | Self::Opening | Self::Open => true,
            Self::Received(_, claimed) | Self::Awaiting(_, claimed) => claimed != source,
            _ => false,
        }
    }
}

#[derive(Clone, Copy)]
struct Context {
    index: usize,
    hart: u64,
    inherited: u32,
    threshold: u32,
    state: State,
}

/// Resident storage for one physical supervisor context.
pub struct ContextSlot(Option<Context>);

impl ContextSlot {
    pub const EMPTY: Self = Self(None);

    pub(super) fn occupied(&self) -> bool {
        self.0.is_some()
    }
}

/// The original PLIC Context paired with its live Hart reception line.
#[must_use]
pub struct Receiver<'scope, 'access, 'storage, 'tree> {
    owner: &'scope mut Access<'access, 'storage, 'tree>,
    local: &'scope Reception,
    slot: usize,
}

impl Sealed for Receiver<'_, '_, '_, '_> {}

impl<'access, 'storage, 'tree> Receive for Receiver<'_, 'access, 'storage, 'tree> {
    type Error = Failure;
    type Claim<'receiver>
        = Claim<'receiver, 'access, 'storage, 'tree>
    where
        Self: 'receiver;

    fn receive(&mut self) -> Result<Option<Self::Claim<'_>>, Failure> {
        self.owner.claim_slot(self.slot, self.local)
    }

    fn pending_claim(&mut self) -> Result<Option<Self::Claim<'_>>, Failure> {
        self.owner.current_slot(self.slot, self.local)
    }
}

/// An exact borrow of the context that performed the original claim read.
#[must_use]
pub struct Claim<'slot, 'access, 'storage, 'tree> {
    owner: &'slot mut Access<'access, 'storage, 'tree>,
    local: &'slot Reception,
    slot: usize,
    round: u64,
    source: u32,
}

impl Claim<'_, '_, '_, '_> {
    pub fn round(&self) -> u64 {
        self.round
    }

    pub fn source(&self) -> u32 {
        self.source
    }

    pub fn expected(&self) -> bool {
        let context = self.owner.controller.contexts[self.slot].0.unwrap();
        self.owner.delivered_by_route(self.source, context.index)
    }

    /// # Safety
    /// The actual claimed source's owner, including for an unexpected claim,
    /// has serviced or quarantined this event. Its original notification
    /// has settled or has durable storage elsewhere.
    pub unsafe fn complete(&mut self) -> Result<(), Failure> {
        let context = self.owner.controller.contexts[self.slot].0.unwrap();
        self.owner.check(context, self.local)?;
        if context.state != State::Received(self.round, self.source) {
            return Err(Failure::Busy);
        }
        if self.source > self.owner.controller.domain.sources {
            return Err(Failure::Scope);
        }
        let (register, mask) = enable(context.index, self.source);
        if self.owner.read(register) & mask == 0 {
            return Err(Failure::Unconfirmed);
        }
        self.owner
            .set_context(self.slot, State::Completing(self.round, self.source));
        self.owner.write(claim(context.index), self.source);
        self.owner
            .set_context(self.slot, State::Awaiting(self.round, self.source));
        Ok(())
    }

    /// # Safety
    /// Supply the platform's evidence that this exact completion write
    /// reached the original enabled context and was accepted by its gateway.
    pub unsafe fn confirm(&mut self) -> Result<(), Failure> {
        let context = self.owner.controller.contexts[self.slot].0.unwrap();
        self.owner.check(context, self.local)?;
        if context.state != State::Awaiting(self.round, self.source) {
            return Err(Failure::Busy);
        }
        self.owner.set_context(self.slot, State::Open);
        Ok(())
    }
}

impl<'access, 'storage, 'tree> Access<'access, 'storage, 'tree> {
    /// Borrow the exact prepared controller context and its original Hart
    /// line for receive and reacquisition without passing context numbers.
    pub fn receiver<'scope>(
        &'scope mut self,
        destination: Destination<'tree>,
        local: &'scope Reception,
    ) -> Result<Receiver<'scope, 'access, 'storage, 'tree>, Failure> {
        if destination.domain.node != self.controller.domain.node {
            return Err(Failure::Scope);
        }
        let slot = self.lookup_context(destination.index)?;
        let context = self.controller.contexts[slot].0.unwrap();
        self.check(context, local)?;
        if context.hart != destination.hart {
            return Err(Failure::Scope);
        }
        if !matches!(
            context.state,
            State::Open | State::Received(..) | State::Awaiting(..)
        ) {
            return Err(Failure::Busy);
        }
        Ok(Receiver {
            owner: self,
            local,
            slot,
        })
    }

    /// # Safety
    /// The previous context has drained all claims and retained writes.
    /// The original Hart's reception line stays masked until its owner
    /// opens this context using the corresponding reception authority.
    pub unsafe fn prepare(
        &mut self,
        destination: Destination<'tree>,
        requested_threshold: u32,
    ) -> Result<(), Failure> {
        if destination.domain.node != self.controller.domain.node {
            return Err(Failure::Scope);
        }
        self.controller
            .limits
            .check_threshold(requested_threshold)?;
        let mut context = Context {
            index: destination.index,
            hart: destination.hart,
            inherited: 0,
            threshold: requested_threshold,
            state: State::Prepared,
        };
        let slot = match self.lookup_context(context.index) {
            Ok(slot) if self.controller.contexts[slot].0.unwrap().state == State::Shut => slot,
            Ok(_) => return Err(Failure::Busy),
            Err(_) => self
                .controller
                .contexts
                .iter()
                .position(|slot| slot.0.is_none())
                .ok_or(Failure::Capacity)?,
        };
        for word in 0..=(self.controller.domain.sources as usize / 32) {
            if self.read(enables(context.index) + 4 * word) != 0 {
                return Err(Failure::Busy);
            }
        }
        context.inherited = self.read(threshold(context.index));
        self.controller.contexts[slot].0 = Some(context);
        Ok(())
    }

    fn lookup_context(&self, index: usize) -> Result<usize, Failure> {
        self.controller
            .contexts
            .iter()
            .position(|slot| slot.0.is_some_and(|context| context.index == index))
            .ok_or(Failure::Scope)
    }

    fn check(&self, context: Context, local: &Reception) -> Result<(), Failure> {
        (local.valid() && local.hart() == context.hart && local.cause() == Cause::External)
            .then_some(())
            .ok_or(Failure::Scope)
    }

    pub fn state(&self, index: usize) -> Result<State, Failure> {
        Ok(self.controller.contexts[self.lookup_context(index)?]
            .0
            .unwrap()
            .state)
    }

    pub(super) fn holds_claim(&self, id: u32) -> bool {
        self.controller.contexts.iter().any(|slot| {
            slot.0
                .is_some_and(|context| context.state.retains_claim(id))
        })
    }

    fn set_context(&mut self, slot: usize, state: State) {
        self.controller.contexts[slot].0.as_mut().unwrap().state = state;
    }

    /// # Safety
    /// A trusted bounded receiver is ready. Retry an opening for the same
    /// original context or close it with the corresponding drain evidence.
    pub unsafe fn open(&mut self, index: usize, local: &mut Reception) -> Result<(), Failure> {
        let slot = self.lookup_context(index)?;
        let context = self.controller.contexts[slot].0.unwrap();
        self.check(context, local)?;
        if !matches!(context.state, State::Prepared | State::Opening)
            || (context.state == State::Prepared && local.enabled())
        {
            return Err(Failure::Busy);
        }
        self.set_context(slot, State::Opening);
        self.program(threshold(index), context.threshold)?;
        local.set(true);
        if !local.enabled() {
            return Err(Failure::Unconfirmed);
        }
        self.set_context(slot, State::Open);
        Ok(())
    }

    pub fn claim<'slot>(
        &'slot mut self,
        index: usize,
        local: &'slot Reception,
    ) -> Result<Option<Claim<'slot, 'access, 'storage, 'tree>>, Failure> {
        let slot = self.lookup_context(index)?;
        self.claim_slot(slot, local)
    }

    fn claim_slot<'slot>(
        &'slot mut self,
        slot: usize,
        local: &'slot Reception,
    ) -> Result<Option<Claim<'slot, 'access, 'storage, 'tree>>, Failure> {
        let context = self.controller.contexts[slot].0.unwrap();
        self.check(context, local)?;
        if context.state != State::Open {
            return Err(Failure::Busy);
        }
        let round = self.controller.next_round;
        self.controller.next_round = round.checked_add(1).ok_or(Failure::Exhausted)?;
        self.set_context(slot, State::Receiving(round));
        let source = self.read(claim(context.index));
        if source == 0 {
            self.set_context(slot, State::Open);
            return Ok(None);
        }
        self.set_context(slot, State::Received(round, source));
        self.current_slot(slot, local)
    }

    pub fn current<'slot>(
        &'slot mut self,
        index: usize,
        local: &'slot Reception,
    ) -> Result<Option<Claim<'slot, 'access, 'storage, 'tree>>, Failure> {
        let slot = self.lookup_context(index)?;
        self.current_slot(slot, local)
    }

    fn current_slot<'slot>(
        &'slot mut self,
        slot: usize,
        local: &'slot Reception,
    ) -> Result<Option<Claim<'slot, 'access, 'storage, 'tree>>, Failure> {
        let context = self.controller.contexts[slot].0.unwrap();
        self.check(context, local)?;
        let Some((round, source)) = context.state.reborrowable_claim()? else {
            return Ok(None);
        };
        Ok(Some(Claim {
            owner: self,
            local,
            slot,
            round,
            source,
        }))
    }

    /// # Safety
    /// The original context and gateway effects have settled. Local
    /// masking and restoring the threshold cannot prove that by themselves.
    pub unsafe fn shut(&mut self, index: usize, local: &mut Reception) -> Result<(), Failure> {
        let slot = self.lookup_context(index)?;
        let context = self.controller.contexts[slot].0.unwrap();
        self.check(context, local)?;
        if !matches!(
            context.state,
            State::Prepared | State::Opening | State::Open | State::Shutting
        ) || self.has_routes(index)
        {
            return Err(Failure::Busy);
        }
        self.set_context(slot, State::Shutting);
        local.set(false);
        if local.enabled() {
            return Err(Failure::Unconfirmed);
        }
        self.program(threshold(index), context.inherited)?;
        self.set_context(slot, State::Shut);
        Ok(())
    }
    /// Release the storage for an already retired context.
    pub fn detach(&mut self, index: usize) -> Result<(), Failure> {
        let slot = self.lookup_context(index)?;
        if self.controller.contexts[slot].0.unwrap().state != State::Shut {
            return Err(Failure::Busy);
        }
        self.controller.contexts[slot].0 = None;
        Ok(())
    }

    /// Release a prepared context that never opened and has no routes.
    pub fn cancel(&mut self, index: usize) -> Result<(), Failure> {
        let slot = self.lookup_context(index)?;
        if self.controller.contexts[slot].0.unwrap().state != State::Prepared
            || self.has_routes(index)
        {
            return Err(Failure::Busy);
        }
        self.controller.contexts[slot].0 = None;
        Ok(())
    }
}

#[cfg(test)]
#[path = "../../../../../../tests/metal/interrupt/riscv64/external/plic/context.rs"]
mod tests;

impl crate::metal::interrupt::treaty::claim::Sealed for Claim<'_, '_, '_, '_> {}

impl HardwareClaim for Claim<'_, '_, '_, '_> {
    type Error = Failure;

    fn identity(&self) -> ClaimId {
        let controller = &self.owner.controller;
        let context = controller.contexts[self.slot].0.unwrap();
        ClaimId::new(
            controller.domain.registers().start(),
            context.index as u64,
            self.round,
            self.source,
        )
    }

    fn confirm(&mut self, evidence: ClaimConfirmation) -> Result<Completion, Failure> {
        if evidence.id() != self.identity() {
            return Err(Failure::Scope);
        }
        let context = self.owner.controller.contexts[self.slot].0.unwrap();
        self.owner.check(context, self.local)?;
        if context.state != State::Awaiting(self.round, self.source) {
            return Err(Failure::Busy);
        }
        if !evidence.accepted() {
            return Ok(Completion::AwaitingAcceptance);
        }
        unsafe { Claim::confirm(self) }.map(|()| Completion::Settled)
    }

    unsafe fn finish(&mut self) -> Result<Completion, Failure> {
        unsafe { self.complete() }.map(|()| Completion::AwaitingAcceptance)
    }
}
