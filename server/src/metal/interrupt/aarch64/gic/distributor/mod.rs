//! One Distributor serializes shared register words for independent SPI routes.

use super::{GicV3, Trigger};

mod route;
use crate::metal::{
    interrupt::treaty::{RouteControl, RouteOperation, RouteState},
    memory::access::treaty::device::DeviceWindow,
    processor::description::aarch64::cpu::AFFINITY_MASK,
};
pub use route::Route;

const SPI_LIMIT: usize = 988;
const CTLR: usize = 0;
const TYPER: usize = 4;
const PIDR2: usize = 0xffe8;
const GROUP: usize = 0x80;
const ENABLE: usize = 0x100;
const DISABLE: usize = 0x180;
const PENDING: usize = 0x200;
const ACTIVE: usize = 0x300;
const PRIORITY: usize = 0x400;
const CONFIG: usize = 0xc00;
const ROUTER: usize = 0x6000;
const AFFINITY_ROUTING: u32 = 1 << 4;
const SINGLE_SECURITY_STATE: u32 = 1 << 6;
const WRITE_PENDING: u32 = 1 << 31;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Failure {
    Mapping,
    Profile,
    Capacity,
    Busy,
    State,
    Unconfirmed,
}

/// Only the exact affinity and priority can retry an uncertain configure.
/// An uncertain enable can be masked. An uncertain mask can be observed again.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum State {
    Masked,
    Configuring { affinity: u64, priority: u8 },
    Configured,
    Enabling,
    Resuming,
    Enabled,
    Masking,
    Quiesced,
}

/// Bounded storage for one live SPI. Keep it with the Distributor until
/// every admitted route has been retired.
pub struct Slot<'tree> {
    route: Option<(Route<'tree>, State)>,
}

impl Slot<'_> {
    pub const EMPTY: Self = Self { route: None };
}

/// The exclusive reference to an admitted SPI binding. Keep it until the
/// original route is retired, even when the Distributor is lent elsewhere.
#[must_use]
pub struct BoundRoute<'tree> {
    route: Option<Route<'tree>>,
    slot: usize,
}

/// Ordering for an admitted GIC Distributor window.
pub struct Barrier {
    private: (),
}

impl Barrier {
    #[cfg(all(limen_target, target_arch = "aarch64"))]
    pub const fn native() -> Self {
        Self { private: () }
    }

    fn synchronize(&self) {
        let Self { private: () } = self;
        #[cfg(all(limen_target, target_arch = "aarch64"))]
        unsafe {
            core::arch::asm!("dsb sy", options(nostack, preserves_flags));
        }
        #[cfg(not(all(limen_target, target_arch = "aarch64")))]
        unreachable!("GIC ordering requires an admitted AArch64 target");
    }
}

#[must_use]
pub struct Distributor<'map, 'tree> {
    mapping: DeviceWindow<'map>,
    gic: GicV3<'tree>,
    barrier: Barrier,
    slots: &'map mut [Slot<'tree>],
    count: usize,
}

struct Control<'route, 'map, 'tree> {
    owner: &'route mut Distributor<'map, 'tree>,
    binding: &'route mut BoundRoute<'tree>,
}

impl crate::metal::interrupt::treaty::route::Sealed for Control<'_, '_, '_> {}

impl RouteControl for Control<'_, '_, '_> {
    type Failure = Failure;
    type Retired = ();

    fn state(&self) -> RouteState {
        match self.owner.slots[self.binding.slot]
            .route
            .map(|(_, state)| state)
        {
            Some(State::Configured) => RouteState::Prepared,
            Some(State::Enabling) => RouteState::Unconfirmed(RouteOperation::Publish),
            Some(State::Resuming) => RouteState::Unconfirmed(RouteOperation::Resume),
            Some(State::Enabled) => RouteState::Open,
            Some(State::Masking) => RouteState::Unconfirmed(RouteOperation::Mask),
            Some(State::Quiesced) => RouteState::Masked,
            Some(State::Masked | State::Configuring { .. }) => {
                unreachable!("SPI control requires a configured route")
            }
            None => RouteState::Retired,
        }
    }

    unsafe fn publish(&mut self) -> Result<(), Failure> {
        let slot = self.owner.bound_slot(self.binding)?;
        unsafe { self.owner.enable(slot, State::Configured, State::Enabling) }
    }

    fn mask(&mut self) -> Result<(), Failure> {
        let slot = self.owner.bound_slot(self.binding)?;
        self.owner.disable(slot)
    }

    unsafe fn resume(&mut self) -> Result<(), Failure> {
        let slot = self.owner.bound_slot(self.binding)?;
        unsafe { self.owner.enable(slot, State::Quiesced, State::Resuming) }
    }

    unsafe fn retire(&mut self) -> Result<(), Failure> {
        let slot = self.owner.bound_slot(self.binding)?;
        unsafe { self.owner.retire(slot) }?;
        self.binding.route = None;
        Ok(())
    }
}

impl<'map, 'tree> Distributor<'map, 'tree> {
    /// # Safety
    /// The mapping is the original little-endian GICD in Non-secure state.
    /// The caller excludes other writers of the shared Distributor words.
    /// Existing firmware sources outside the admitted routes remain owned
    /// by the original party. Keep the owner, slots and mapping live until
    /// all admitted sources and their effects settle.
    pub unsafe fn admit(
        gic: GicV3<'tree>,
        mapping: DeviceWindow<'map>,
        barrier: Barrier,
        slots: &'map mut [Slot<'tree>],
    ) -> Result<Self, Failure> {
        if mapping.physical() != gic.distributor() || !mapping.supports_u32(PIDR2) {
            return Err(Failure::Mapping);
        }
        if slots.is_empty() || slots.len() > SPI_LIMIT {
            return Err(Failure::Capacity);
        }
        if slots.iter().any(|slot| slot.route.is_some()) {
            return Err(Failure::Busy);
        }
        let mut distributor = Self {
            mapping,
            gic,
            barrier,
            slots,
            count: 0,
        };
        let revision = distributor.word(PIDR2) >> 4 & 0xf;
        distributor.count = 32 * (1 + (distributor.word(TYPER) & 31)) as usize;
        if !matches!(revision, 3 | 4) || distributor.word(CTLR) & AFFINITY_ROUTING == 0 {
            return Err(Failure::Profile);
        }
        Ok(distributor)
    }

    /// # Safety
    /// The caller has quiesced this source and settled previous receptions
    /// and writes. In a two-security-state domain trusted firmware must grant
    /// this Non-secure Group 1 SPI as the group register is Secure-only.
    pub unsafe fn reserve(&mut self, route: Route<'tree>) -> Result<BoundRoute<'tree>, Failure> {
        if route.controller.source() != self.gic.source() || route.intid >= self.count {
            return Err(Failure::Profile);
        }
        let id = route.intid;
        if self.lookup(id).is_ok() || !self.available(id) {
            return Err(Failure::Busy);
        }
        let slot = self
            .slots
            .iter()
            .position(|slot| slot.route.is_none())
            .ok_or(Failure::Capacity)?;
        self.slots[slot].route = Some((route, State::Masked));
        Ok(BoundRoute {
            route: Some(route),
            slot,
        })
    }

    fn lookup(&self, id: usize) -> Result<usize, Failure> {
        self.slots
            .iter()
            .position(|slot| slot.route.is_some_and(|(route, _)| route.intid == id))
            .ok_or(Failure::State)
    }

    fn bound_slot(&self, binding: &BoundRoute<'tree>) -> Result<usize, Failure> {
        let route = binding.route.ok_or(Failure::State)?;
        let (original, _) = self
            .slots
            .get(binding.slot)
            .and_then(|slot| slot.route)
            .ok_or(Failure::State)?;
        if route.controller.source() != self.gic.source()
            || route.intid != original.intid
            || route.device() != original.device()
        {
            return Err(Failure::Profile);
        }
        Ok(binding.slot)
    }

    /// Recover the original route control after using a different route.
    pub fn control<'route>(
        &'route mut self,
        binding: &'route mut BoundRoute<'tree>,
    ) -> Result<impl RouteControl<Failure = Failure, Retired = ()> + 'route, Failure> {
        let slot = self.bound_slot(binding)?;
        if matches!(
            self.slots[slot].route.unwrap().1,
            State::Masked | State::Configuring { .. }
        ) {
            return Err(Failure::State);
        }
        Ok(Control {
            owner: self,
            binding,
        })
    }

    /// Return an unprogrammed reservation to its original Distributor.
    pub fn cancel(&mut self, binding: &mut BoundRoute<'tree>) -> Result<(), Failure> {
        let slot = self.bound_slot(binding)?;
        if self.slots[slot].route.unwrap().1 != State::Masked {
            return Err(Failure::State);
        }
        self.slots[slot].route = None;
        binding.route = None;
        Ok(())
    }

    pub fn state(&self, id: usize) -> Result<State, Failure> {
        Ok(self.slots[self.lookup(id)?].route.unwrap().1)
    }

    /// Observe the physical active bit without acknowledging reception.
    pub fn active(&mut self, id: usize) -> Result<bool, Failure> {
        self.lookup(id)?;
        Ok(self.bit(ACTIVE, id))
    }

    fn word(&mut self, offset: usize) -> u32 {
        u32::from_le(unsafe { self.mapping.read_u32(offset) }.expect("checked GICD window"))
    }

    fn write(&mut self, offset: usize, bits: u32) {
        unsafe { self.mapping.write_u32(offset, bits.to_le()) }.expect("checked GICD window");
    }

    fn bank(offset: usize, id: usize) -> usize {
        offset + id / 32 * 4
    }

    fn mask(id: usize) -> u32 {
        1 << (id % 32)
    }

    fn bit(&mut self, offset: usize, id: usize) -> bool {
        self.word(Self::bank(offset, id)) & Self::mask(id) != 0
    }

    fn available(&mut self, id: usize) -> bool {
        self.word(CTLR) & WRITE_PENDING == 0
            && !self.bit(ENABLE, id)
            && !self.bit(ACTIVE, id)
            && !self.bit(PENDING, id)
    }

    /// Configure the admitted SPI and return its original publication borrow.
    /// The confirmed configuration stays in the Distributor when the borrow ends.
    ///
    /// # Safety
    /// The admitted source remains quiescent while its route is changed.
    /// Affinity names a present PE in the same domain whose Group 1
    /// reception will be available before unmasking.
    pub unsafe fn configure<'route>(
        &'route mut self,
        binding: &'route mut BoundRoute<'tree>,
        affinity: u64,
        priority: u8,
    ) -> Result<impl RouteControl<Failure = Failure, Retired = ()> + 'route, Failure> {
        let slot = self.bound_slot(binding)?;
        let (route, state) = self.slots[slot].route.unwrap();
        let id = route.intid;
        let operation = State::Configuring { affinity, priority };
        if state != State::Masked && state != operation {
            return Err(Failure::State);
        }
        if affinity & !AFFINITY_MASK != 0 {
            return Err(Failure::Profile);
        }
        if !self.available(id) {
            return Err(Failure::Busy);
        }
        self.slots[slot].route = Some((route, operation));
        if self.word(CTLR) & SINGLE_SECURITY_STATE != 0 {
            let group = Self::bank(GROUP, id);
            let value = self.word(group) | Self::mask(id);
            self.write(group, value);
            self.barrier.synchronize();
            if !self.bit(GROUP, id) {
                return Err(Failure::Unconfirmed);
            }
        }
        let config = CONFIG + id / 16 * 4;
        let edge = 2 << (id % 16 * 2);
        let wanted = if route.trigger == Trigger::EdgeRising {
            edge
        } else {
            0
        };
        let bits = (self.word(config) & !edge) | wanted;
        self.write(config, bits);
        unsafe {
            self.mapping
                .write_u8(PRIORITY + id, priority)
                .expect("checked GICD priority");
            self.mapping
                .write_u64(ROUTER + id * 8, affinity.to_le())
                .expect("checked GICD route");
        }
        self.barrier.synchronize();
        if self.word(config) & edge != wanted
            || unsafe { self.mapping.read_u8(PRIORITY + id) }.expect("checked GICD priority")
                != priority
            || u64::from_le(
                unsafe { self.mapping.read_u64(ROUTER + id * 8) }.expect("checked GICD route"),
            ) != affinity
        {
            return Err(Failure::Unconfirmed);
        }
        self.slots[slot].route = Some((route, State::Configured));
        self.control(binding)
    }

    /// # Safety
    /// The original target can receive this route with durable capacity
    /// to retain the matching IAR, priority drop and deactivation.
    unsafe fn enable(&mut self, slot: usize, ready: State, pending: State) -> Result<(), Failure> {
        let (route, state) = self.slots[slot].route.unwrap();
        let id = route.intid;
        if state == ready {
            self.slots[slot].route = Some((route, pending));
            self.write(Self::bank(ENABLE, id), Self::mask(id));
            self.barrier.synchronize();
        } else if state != pending {
            return Err(Failure::State);
        }
        if !self.bit(ENABLE, id) {
            return Err(Failure::Unconfirmed);
        }
        self.slots[slot].route = Some((route, State::Enabled));
        Ok(())
    }

    /// Mask this source. Success still leaves old reception obligations.
    fn disable(&mut self, slot: usize) -> Result<(), Failure> {
        let (route, state) = self.slots[slot].route.unwrap();
        let id = route.intid;
        match state {
            State::Quiesced => return Ok(()),
            State::Enabled | State::Enabling | State::Resuming => {
                self.slots[slot].route = Some((route, State::Masking));
                self.write(Self::bank(DISABLE, id), Self::mask(id));
                self.barrier.synchronize();
            }
            State::Masking => {}
            _ => return Err(Failure::State),
        }
        if self.word(CTLR) & WRITE_PENDING != 0 || self.bit(ENABLE, id) {
            return Err(Failure::Busy);
        }
        self.slots[slot].route = Some((route, State::Quiesced));
        Ok(())
    }

    /// # Safety
    /// Exclude future assertions and old writes, and settle the original
    /// source and CPU interface reception. Distributor bits cannot prove
    /// the separately owned software and device obligations.
    unsafe fn retire(&mut self, slot: usize) -> Result<(), Failure> {
        let (route, state) = self.slots[slot].route.unwrap();
        if !matches!(state, State::Configured | State::Quiesced) || !self.available(route.intid) {
            return Err(Failure::Busy);
        }
        self.slots[slot].route = None;
        Ok(())
    }
}
