//! One physical domain serializes shared enable words and retains claims.

use super::{Domain, Route, enable, enables, priority, threshold};
use crate::metal::{
    interrupt::riscv64::{
        external::Barrier,
        internal::supervisor::{Cause, Reception},
    },
    memory::mmu::access::device::DeviceWindow,
};
use limen_treaty::model::Alignment;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Failure {
    Scope,
    Busy,
    Capacity,
    Exhausted,
    Unconfirmed,
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
    fn excludes_source(self, source: u32) -> bool {
        match self {
            Self::Prepared | Self::Opening | Self::Open => true,
            Self::Received(_, claimed) | Self::Awaiting(_, claimed) => claimed != source,
            _ => false,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceState {
    Prepared,
    Enabling,
    Enabled,
    Disabling,
}

#[derive(Clone, Copy)]
struct Context {
    index: usize,
    hart: u64,
    threshold: u32,
    state: State,
}

/// Caller-supplied storage for one independent physical context.
pub struct ContextSlot(Option<Context>);

impl ContextSlot {
    pub const EMPTY: Self = Self(None);
}

#[derive(Clone, Copy)]
struct Source {
    id: u32,
    context: usize,
    priority: u32,
    state: SourceState,
}

/// Caller-supplied storage for one physical source and its priority obligation.
pub struct SourceSlot(Option<Source>);

impl SourceSlot {
    pub const EMPTY: Self = Self(None);
}

/// The persistent owner of one domain's context, source and claim records.
/// The owner can be shared under the composition's actual serialization.
#[must_use]
pub struct Controller<'storage, 'tree> {
    domain: Domain<'tree>,
    contexts: &'storage mut [ContextSlot],
    sources: &'storage mut [SourceSlot],
    next_round: u64,
}

/// Exclusive short-lived access using the executing Hart's own MMIO window.
/// Releasing access does not release any route, context or received claim.
#[must_use]
pub struct Access<'access, 'storage, 'tree> {
    controller: &'access mut Controller<'storage, 'tree>,
    mapping: &'access mut DeviceWindow,
    barrier: Barrier,
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
        self.owner
            .lookup_source(self.source)
            .ok()
            .is_some_and(|slot| {
                self.owner.controller.sources[slot].0.is_some_and(|source| {
                    source.context == context.index && source.state == SourceState::Enabled
                })
            })
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
        self.owner.write(threshold(context.index) + 4, self.source);
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

impl<'storage, 'tree> Controller<'storage, 'tree> {
    /// # Safety
    /// The caller owns this domain and excludes other writers in admitted
    /// contexts and admitted source priorities. Retain that authority and
    /// the original records until all effects settle, including failures.
    pub unsafe fn admit(
        domain: Domain<'tree>,
        contexts: &'storage mut [ContextSlot],
        sources: &'storage mut [SourceSlot],
    ) -> Result<Self, Failure> {
        if contexts.is_empty()
            || contexts.len() > domain.contexts
            || sources.is_empty()
            || sources.len() > domain.sources as usize
        {
            return Err(Failure::Scope);
        }
        if contexts.iter().any(|slot| slot.0.is_some())
            || sources.iter().any(|slot| slot.0.is_some())
        {
            return Err(Failure::Busy);
        }
        Ok(Self {
            domain,
            contexts,
            sources,
            next_round: 1,
        })
    }

    /// Borrow only the executing Hart's MMIO window. Neither a window for
    /// another domain nor a second concurrent access can enter this owner.
    ///
    /// # Safety
    /// The ordered enable-word readback must complete preceding writes to
    /// that word on this interconnect, even when the value differs from the
    /// one requested. A later access may update the same word on another Hart.
    pub unsafe fn access<'access>(
        &'access mut self,
        mapping: &'access mut DeviceWindow,
        barrier: Barrier,
    ) -> Result<Access<'access, 'storage, 'tree>, Failure> {
        if self.domain.registers() != mapping.physical()
            || !mapping.is_aligned(Alignment::new(4096).unwrap())
        {
            return Err(Failure::Scope);
        }
        Ok(Access {
            controller: self,
            mapping,
            barrier,
        })
    }
}

impl<'access, 'storage, 'tree> Access<'access, 'storage, 'tree> {
    /// # Safety
    /// The previous context has drained all claims and retained words; the
    /// caller owns this original Hart's reception line and threshold.
    pub unsafe fn attach(&mut self, route: Route<'tree>, local: &Reception) -> Result<(), Failure> {
        self.check_route(route)?;
        let mut context = Context {
            index: route.index,
            hart: route.hart,
            threshold: 0,
            state: State::Prepared,
        };
        self.check(context, local)?;
        if local.enabled() {
            return Err(Failure::Busy);
        }
        let slot = match self.lookup_context(route.index) {
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
        context.threshold = self.read(threshold(context.index));
        self.controller.contexts[slot].0 = Some(context);
        Ok(())
    }

    /// # Safety
    /// The stopped device and its original gateway are exclusive to this
    /// binding, including priority. Other contexts exclude this source and
    /// prior deliveries and writes have settled.
    pub unsafe fn reserve(&mut self, route: Route<'tree>) -> Result<(), Failure> {
        self.check_route(route)?;
        let context = self.lookup_context(route.index)?;
        if !matches!(
            self.controller.contexts[context].0.unwrap().state,
            State::Prepared | State::Open
        ) || self.lookup_source(route.source).is_ok()
        {
            return Err(Failure::Busy);
        }
        let slot = self
            .controller
            .sources
            .iter()
            .position(|slot| slot.0.is_none())
            .ok_or(Failure::Capacity)?;
        let priority = self.read(priority(route.source));
        self.controller.sources[slot].0 = Some(Source {
            id: route.source,
            context: route.index,
            priority,
            state: SourceState::Prepared,
        });
        Ok(())
    }

    fn lookup_context(&self, index: usize) -> Result<usize, Failure> {
        self.controller
            .contexts
            .iter()
            .position(|slot| slot.0.is_some_and(|context| context.index == index))
            .ok_or(Failure::Scope)
    }

    fn lookup_source(&self, id: u32) -> Result<usize, Failure> {
        self.controller
            .sources
            .iter()
            .position(|slot| slot.0.is_some_and(|source| source.id == id))
            .ok_or(Failure::Scope)
    }

    fn check_route(&self, route: Route<'tree>) -> Result<(), Failure> {
        (route.domain.node == self.controller.domain.node)
            .then_some(())
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

    pub fn source(&self, id: u32) -> Result<SourceState, Failure> {
        Ok(self.controller.sources[self.lookup_source(id)?]
            .0
            .unwrap()
            .state)
    }

    fn set_context(&mut self, slot: usize, state: State) {
        self.controller.contexts[slot].0.as_mut().unwrap().state = state;
    }

    fn read(&mut self, offset: usize) -> u32 {
        self.barrier.synchronize();
        let bits = unsafe { self.mapping.read_u32(offset) }.expect("admitted PLIC window");
        self.barrier.synchronize();
        bits
    }

    fn write(&mut self, offset: usize, value: u32) {
        self.barrier.synchronize();
        unsafe { self.mapping.write_u32(offset, value) }.expect("admitted PLIC window");
        self.barrier.synchronize();
    }

    fn program(&mut self, offset: usize, value: u32) -> Result<(), Failure> {
        self.write(offset, value);
        (self.read(offset) == value)
            .then_some(())
            .ok_or(Failure::Unconfirmed)
    }

    fn set_enabled(&mut self, source: Source, enabled: bool) -> Result<(), Failure> {
        let (register, mask) = enable(source.context, source.id);
        let current = self.read(register);
        self.program(register, change_enable(current, mask, enabled))
    }

    /// # Safety
    /// A compatible receiver and the source's handler have been prepared.
    /// Delivery through an open context may follow immediately.
    pub unsafe fn enable(&mut self, id: u32) -> Result<(), Failure> {
        let slot = self.lookup_source(id)?;
        let source = self.controller.sources[slot].0.unwrap();
        if !matches!(source.state, SourceState::Prepared | SourceState::Enabling)
            || !self.state(source.context)?.excludes_source(id)
        {
            return Err(Failure::Busy);
        }
        self.controller.sources[slot].0.as_mut().unwrap().state = SourceState::Enabling;
        self.program(priority(id), 1)?;
        self.set_enabled(source, true)?;
        self.controller.sources[slot].0.as_mut().unwrap().state = SourceState::Enabled;
        Ok(())
    }

    /// # Safety
    /// A trusted bounded receiver is ready. Retry an opening for the same
    /// original context or close it with the corresponding drain evidence.
    pub unsafe fn open(&mut self, index: usize, local: &mut Reception) -> Result<(), Failure> {
        let slot = self.lookup_context(index)?;
        let context = self.controller.contexts[slot].0.unwrap();
        self.check(context, local)?;
        if !matches!(context.state, State::Prepared | State::Opening) {
            return Err(Failure::Busy);
        }
        self.set_context(slot, State::Opening);
        self.program(threshold(index), 0)?;
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
        let context = self.controller.contexts[slot].0.unwrap();
        self.check(context, local)?;
        if context.state != State::Open {
            return Err(Failure::Busy);
        }
        let round = self.controller.next_round;
        self.controller.next_round = round.checked_add(1).ok_or(Failure::Exhausted)?;
        self.set_context(slot, State::Receiving(round));
        let source = self.read(threshold(index) + 4);
        if source == 0 {
            self.set_context(slot, State::Open);
            return Ok(None);
        }
        self.set_context(slot, State::Received(round, source));
        self.current(index, local)
    }

    pub fn current<'slot>(
        &'slot mut self,
        index: usize,
        local: &'slot Reception,
    ) -> Result<Option<Claim<'slot, 'access, 'storage, 'tree>>, Failure> {
        let slot = self.lookup_context(index)?;
        let context = self.controller.contexts[slot].0.unwrap();
        self.check(context, local)?;
        let (State::Received(round, source) | State::Awaiting(round, source)) = context.state
        else {
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
    /// The device, original deliveries and previous writes are drained and
    /// future assertions from the retiring binding are excluded.
    pub unsafe fn retire(&mut self, id: u32) -> Result<(), Failure> {
        let slot = self.lookup_source(id)?;
        let source = self.controller.sources[slot].0.unwrap();
        if !self.state(source.context)?.excludes_source(id) {
            return Err(Failure::Busy);
        }
        self.controller.sources[slot].0.as_mut().unwrap().state = SourceState::Disabling;
        self.set_enabled(source, false)?;
        self.program(priority(id), source.priority)?;
        self.controller.sources[slot].0 = None;
        Ok(())
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
        ) || self
            .controller
            .sources
            .iter()
            .any(|slot| slot.0.is_some_and(|source| source.context == index))
        {
            return Err(Failure::Busy);
        }
        self.set_context(slot, State::Shutting);
        local.set(false);
        if local.enabled() {
            return Err(Failure::Unconfirmed);
        }
        self.program(threshold(index), context.threshold)?;
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
}

fn change_enable(current: u32, mask: u32, enabled: bool) -> u32 {
    if enabled {
        current | mask
    } else {
        current & !mask
    }
}

#[cfg(test)]
#[path = "../../../../../../tests/metal/interrupt/riscv64/external/plic/mod.rs"]
mod tests;
