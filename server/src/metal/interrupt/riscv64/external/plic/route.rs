//! Exclusive route admission, independent of source and context identities.

use super::{
    Access, Destination, Failure, Source, State,
    registers::{enable, priority},
    source::Priority,
};
use crate::metal::{
    interrupt::treaty::{RouteControl, RouteOperation, RouteState as SharedState},
    platform::description::devicetree::Error,
};

const PENDING: usize = 0x1000;

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
    Masking,
    Masked,
    Reprioritizing,
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

/// The exclusive reference to an admitted source and original context.
#[must_use]
pub struct BoundRoute<'tree> {
    route: Option<Route<'tree>>,
    slot: usize,
}

struct Control<'route, 'access, 'storage, 'tree> {
    owner: &'route mut Access<'access, 'storage, 'tree>,
    binding: &'route mut BoundRoute<'tree>,
}

impl crate::metal::interrupt::treaty::route::Sealed for Control<'_, '_, '_, '_> {}

impl RouteControl for Control<'_, '_, '_, '_> {
    type Failure = Failure;
    type Retired = ();

    fn state(&self) -> SharedState {
        let binding = &self.owner.controller.routes[self.binding.slot];
        match binding.0.map(|binding| binding.state) {
            Some(RouteState::Prepared) => SharedState::Prepared,
            Some(RouteState::Prioritizing | RouteState::Enabling) => {
                SharedState::Unconfirmed(RouteOperation::Publish)
            }
            Some(RouteState::Enabled) => SharedState::Open,
            Some(RouteState::Masking) => SharedState::Unconfirmed(RouteOperation::Mask),
            Some(RouteState::Masked) => SharedState::Masked,
            Some(RouteState::Reprioritizing) => SharedState::Unconfirmed(RouteOperation::Resume),
            Some(RouteState::Disabling | RouteState::Restoring) => {
                SharedState::Unconfirmed(RouteOperation::Retire)
            }
            None => SharedState::Retired,
        }
    }

    unsafe fn publish(&mut self) -> Result<(), Failure> {
        let slot = self.owner.bound_slot(self.binding)?;
        unsafe { self.owner.enable(slot, RouteOperation::Publish) }
    }

    fn mask(&mut self) -> Result<(), Failure> {
        let slot = self.owner.bound_slot(self.binding)?;
        self.owner.mask(slot)
    }

    unsafe fn resume(&mut self) -> Result<(), Failure> {
        let slot = self.owner.bound_slot(self.binding)?;
        unsafe { self.owner.enable(slot, RouteOperation::Resume) }
    }

    unsafe fn retire(&mut self) -> Result<(), Failure> {
        let slot = self.owner.bound_slot(self.binding)?;
        unsafe { self.owner.retire(slot) }?;
        self.binding.route = None;
        Ok(())
    }
}

impl<'access, 'storage, 'tree> Access<'access, 'storage, 'tree> {
    /// Reserve the source and return the exclusive construction relation.
    ///
    /// # Safety
    /// The stopped device and its original gateway are exclusive to this
    /// route. Other contexts exclude this source. Old deliveries and writes
    /// have settled. Configuration comes from the admitted hardware profile.
    /// An ordered priority read settles preceding writes to that priority
    /// register on the actual interconnect, including mismatched readback.
    pub unsafe fn reserve(
        &mut self,
        route: Route<'tree>,
        requested_priority: u32,
    ) -> Result<BoundRoute<'tree>, Failure> {
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
        Ok(BoundRoute {
            route: Some(route),
            slot,
        })
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

    fn bound_slot(&self, binding: &BoundRoute<'tree>) -> Result<usize, Failure> {
        let route = binding.route.ok_or(Failure::Scope)?;
        if route.source.domain.node != self.controller.domain.node {
            return Err(Failure::Scope);
        }
        let original = self
            .controller
            .routes
            .get(binding.slot)
            .and_then(|slot| slot.0)
            .ok_or(Failure::Scope)?;
        if route.destination.index != original.context
            || self.controller.sources[original.source].0.unwrap().id != route.source.id
        {
            return Err(Failure::Scope);
        }
        Ok(binding.slot)
    }

    /// Recover the bound route using this access to its original controller.
    pub fn control<'route>(
        &'route mut self,
        binding: &'route mut BoundRoute<'tree>,
    ) -> Result<impl RouteControl<Failure = Failure, Retired = ()> + 'route, Failure> {
        self.bound_slot(binding)?;
        Ok(Control {
            owner: self,
            binding,
        })
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

    pub(super) fn delivered_by_route(&self, id: u32, context: usize) -> bool {
        self.lookup_route(id).is_ok_and(|slot| {
            let binding = self.controller.routes[slot].0.unwrap();
            binding.context == context
                && matches!(
                    binding.state,
                    RouteState::Enabling
                        | RouteState::Enabled
                        | RouteState::Masking
                        | RouteState::Masked
                        | RouteState::Reprioritizing
                )
        })
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
    unsafe fn enable(&mut self, slot: usize, operation: RouteOperation) -> Result<(), Failure> {
        let binding = self.controller.routes[slot].0.unwrap();
        let source = self.controller.sources[binding.source].0.unwrap();
        let id = source.id;
        if !self.state(binding.context)?.excludes_source(id) || self.holds_claim(id) {
            return Err(Failure::Busy);
        }
        match (operation, binding.state) {
            (RouteOperation::Publish, RouteState::Prepared | RouteState::Prioritizing) => {
                self.set_route(slot, RouteState::Prioritizing);
                self.program(priority(id), source.requested)?;
                self.set_route(slot, RouteState::Enabling);
                self.set_enabled(binding.context, id, true)?;
            }
            (RouteOperation::Publish, RouteState::Enabling) => {
                self.set_enabled(binding.context, id, true)?;
            }
            (RouteOperation::Resume, RouteState::Masked | RouteState::Reprioritizing) => {
                self.set_route(slot, RouteState::Reprioritizing);
                self.program(priority(id), source.requested)?;
            }
            _ => return Err(Failure::Busy),
        }
        self.set_route(slot, RouteState::Enabled);
        Ok(())
    }

    fn mask(&mut self, slot: usize) -> Result<(), Failure> {
        let binding = self.controller.routes[slot].0.unwrap();
        match binding.state {
            RouteState::Masked => return Ok(()),
            RouteState::Enabled | RouteState::Masking | RouteState::Reprioritizing => {}
            _ => return Err(Failure::Busy),
        }
        // Only this route owns the global priority. Keep the original IE
        // bit through Complete, including while the claim is still held.
        let id = self.controller.sources[binding.source].0.unwrap().id;
        self.set_route(slot, RouteState::Masking);
        self.program(priority(id), 0)?;
        self.set_route(slot, RouteState::Masked);
        Ok(())
    }

    /// # Safety
    /// Original device assertions, deliveries and prior writes have drained.
    /// Keep excluding the retiring source's new assertions until retirement.
    unsafe fn retire(&mut self, slot: usize) -> Result<(), Failure> {
        let binding = self.controller.routes[slot].0.unwrap();
        let source = self.controller.sources[binding.source].0.unwrap();
        let id = source.id;
        if !matches!(
            binding.state,
            RouteState::Prepared
                | RouteState::Enabled
                | RouteState::Masked
                | RouteState::Disabling
                | RouteState::Restoring
        ) || !self.state(binding.context)?.excludes_source(id)
            || self.holds_claim(id)
            || self.read(PENDING + id as usize / 32 * 4) & (1 << (id % 32)) != 0
        {
            return Err(Failure::Busy);
        }
        if binding.state != RouteState::Prepared {
            if binding.state != RouteState::Restoring {
                self.set_route(slot, RouteState::Disabling);
                self.set_enabled(binding.context, id, false)?;
                self.set_route(slot, RouteState::Restoring);
            }
            self.program(priority(id), source.inherited)?;
        }
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
