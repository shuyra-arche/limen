//! Exclusive route admission, independent of source and context identities.

use super::{
    Access, Destination, Failure, Source, State,
    registers::{enable, priority},
    source::Priority,
};
use crate::metal::platform::description::devicetree::Error;

/// A device source connected to an original supervisor context.
#[derive(Clone, Copy, Debug)]
pub struct Route<'a> {
    source: Source<'a>,
    destination: Destination<'a>,
}

impl<'a> Route<'a> {
    pub fn connect(source: Source<'a>, destination: Destination<'a>) -> Result<Self, Error> {
        if source.domain.node != destination.domain.node {
            return Err(Error::Conflict);
        }
        Ok(Self {
            source,
            destination,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RouteState {
    Prepared,
    Prioritizing,
    Enabling,
    Enabled,
    Disabling,
    Restoring,
}

#[derive(Clone, Copy)]
struct Binding {
    source: usize,
    context: usize,
    state: RouteState,
}

/// Resident storage for one admitted relationship between source and context.
pub struct RouteSlot(Option<Binding>);

impl RouteSlot {
    pub const EMPTY: Self = Self(None);

    pub(super) fn occupied(&self) -> bool {
        self.0.is_some()
    }
}

impl<'access, 'storage, 'tree> Access<'access, 'storage, 'tree> {
    /// # Safety
    /// The stopped device and its original gateway are exclusive to this
    /// route. Other contexts exclude this source. Old deliveries and writes
    /// have settled. Configuration comes from the admitted hardware profile.
    pub unsafe fn reserve(
        &mut self,
        route: Route<'tree>,
        requested_priority: u32,
    ) -> Result<(), Failure> {
        if route.source.domain.node != self.controller.domain.node {
            return Err(Failure::Scope);
        }
        self.controller.limits.check_priority(requested_priority)?;
        if !matches!(
            self.state(route.destination.index)?,
            State::Prepared | State::Open
        ) || self.holds_claim(route.source.id)
        {
            return Err(Failure::Busy);
        }
        let source = exclusive_source_slot(self.controller.sources, route.source.id)?;
        let slot = self
            .controller
            .routes
            .iter()
            .position(|slot| !slot.occupied())
            .ok_or(Failure::Capacity)?;
        let inherited = self.read(priority(route.source.id));
        self.controller.sources[source].0 = Some(Priority {
            id: route.source.id,
            inherited,
            requested: requested_priority,
        });
        self.controller.routes[slot].0 = Some(Binding {
            context: route.destination.index,
            source,
            state: RouteState::Prepared,
        });
        Ok(())
    }

    fn lookup_route(&self, id: u32) -> Result<usize, Failure> {
        self.controller
            .routes
            .iter()
            .position(|slot| {
                slot.0.is_some_and(|binding| {
                    self.controller.sources[binding.source]
                        .0
                        .is_some_and(|source| source.id == id)
                })
            })
            .ok_or(Failure::Scope)
    }

    pub(super) fn has_routes(&self, context: usize) -> bool {
        self.controller
            .routes
            .iter()
            .any(|slot| slot.0.is_some_and(|binding| binding.context == context))
    }

    pub fn route_state(&self, id: u32) -> Result<RouteState, Failure> {
        let slot = self.lookup_route(id)?;
        Ok(self.controller.routes[slot].0.unwrap().state)
    }

    pub(super) fn route_state_at(&self, id: u32, context: usize) -> Result<RouteState, Failure> {
        let slot = self.lookup_route(id)?;
        let binding = self.controller.routes[slot].0.unwrap();
        (binding.context == context)
            .then_some(binding.state)
            .ok_or(Failure::Scope)
    }

    fn set_route(&mut self, slot: usize, state: RouteState) {
        self.controller.routes[slot].0.as_mut().unwrap().state = state;
    }

    fn set_enabled(&mut self, context: usize, id: u32, enabled: bool) -> Result<(), Failure> {
        let (register, mask) = enable(context, id);
        let current = self.read(register);
        self.program(register, change_enable(current, mask, enabled))
    }

    /// # Safety
    /// A compatible receiver and the source's handler are ready. An open
    /// Context may receive the interrupt immediately.
    pub unsafe fn enable(&mut self, id: u32) -> Result<(), Failure> {
        let slot = self.lookup_route(id)?;
        let binding = self.controller.routes[slot].0.unwrap();
        if !matches!(
            binding.state,
            RouteState::Prepared | RouteState::Prioritizing | RouteState::Enabling
        ) || !self.state(binding.context)?.excludes_source(id)
            || self.holds_claim(id)
        {
            return Err(Failure::Busy);
        }
        if binding.state != RouteState::Enabling {
            let source = self.controller.sources[binding.source].0.unwrap();
            self.set_route(slot, RouteState::Prioritizing);
            self.program(priority(id), source.requested)?;
            self.set_route(slot, RouteState::Enabling);
        }
        self.set_enabled(binding.context, id, true)?;
        self.set_route(slot, RouteState::Enabled);
        Ok(())
    }

    /// # Safety
    /// Original device assertions, deliveries and prior writes have drained.
    /// Keep excluding the retiring source's new assertions until retirement.
    pub unsafe fn retire(&mut self, id: u32) -> Result<(), Failure> {
        let slot = self.lookup_route(id)?;
        let binding = self.controller.routes[slot].0.unwrap();
        if !self.state(binding.context)?.excludes_source(id) || self.holds_claim(id) {
            return Err(Failure::Busy);
        }
        if binding.state != RouteState::Restoring {
            self.set_route(slot, RouteState::Disabling);
            self.set_enabled(binding.context, id, false)?;
            self.set_route(slot, RouteState::Restoring);
        }
        let source = self.controller.sources[binding.source].0.unwrap();
        self.program(priority(id), source.inherited)?;
        self.controller.routes[slot].0 = None;
        self.controller.sources[binding.source].0 = None;
        Ok(())
    }
}

fn exclusive_source_slot(sources: &[super::SourceSlot], id: u32) -> Result<usize, Failure> {
    if sources
        .iter()
        .any(|slot| slot.0.is_some_and(|source| source.id == id))
    {
        return Err(Failure::Busy);
    }
    sources
        .iter()
        .position(|slot| !slot.occupied())
        .ok_or(Failure::Capacity)
}

fn change_enable(current: u32, mask: u32, enabled: bool) -> u32 {
    if enabled {
        current | mask
    } else {
        current & !mask
    }
}

#[cfg(test)]
#[path = "../../../../../../tests/metal/interrupt/riscv64/external/plic/route.rs"]
mod tests;
