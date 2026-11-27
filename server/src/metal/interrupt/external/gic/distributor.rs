//! Single-owner programming of one Group 1 SPI with directed affinity.

use super::{Route, Trigger};
use crate::metal::{
    memory::mmu::internal::device::DeviceWindow,
    processor::hart::aarch64::description::AFFINITY_MASK,
};

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
    Busy,
    State,
    Unconfirmed,
}

/// An unconfirmed configuration retains its exact affinity and priority
/// for a same-program retry. An unconfirmed enable can be masked. A mask with writes pending can be polled again by its owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum State {
    Masked,
    Configuring { affinity: u64, priority: u8 },
    Configured,
    Enabling,
    Enabled,
    Masking,
}

pub struct Barrier(fn());

#[cfg(all(limen_target, target_arch = "aarch64"))]
impl Barrier {
    pub const fn native() -> Self {
        Self(|| unsafe { core::arch::asm!("dsb sy", options(nostack, preserves_flags)) })
    }
}

#[must_use]
pub struct Spi<'map, 'tree> {
    mapping: &'map mut DeviceWindow,
    route: Route<'tree>,
    barrier: Barrier,
    state: State,
}

impl<'map, 'tree> Spi<'map, 'tree> {
    /// # Safety
    /// The mapping is the original little-endian GICD in Non-secure state.
    /// The caller holds this SPI, its old targets and all writers sharing
    /// its configuration words excluded for the lifetime of programming.
    /// The source is quiesced, original receptions and changes are settled.
    /// For two security states the trusted firmware handoff explicitly
    /// grants this Non-secure Group 1 source (IGROUPR is Secure-only).
    /// Keep the owner and hardware mapping live until all effects settle.
    pub unsafe fn admit(
        route: Route<'tree>,
        mapping: &'map mut DeviceWindow,
        barrier: Barrier,
    ) -> Result<Self, Failure> {
        if mapping.physical() != route.controller.distributor()
            || !mapping.supports_u64(ROUTER + route.intid * 8)
        {
            return Err(Failure::Mapping);
        }
        let mut spi = Self {
            mapping,
            route,
            barrier,
            state: State::Masked,
        };
        let revision = spi.word(PIDR2) >> 4 & 0xf;
        let count = 32 * (1 + (spi.word(TYPER) & 31));
        let control = spi.word(CTLR);
        if !matches!(revision, 3 | 4)
            || control & AFFINITY_ROUTING == 0
            || route.intid >= count as usize
        {
            return Err(Failure::Profile);
        }
        if control & WRITE_PENDING != 0 || spi.bit(ENABLE) || spi.bit(ACTIVE) || spi.bit(PENDING) {
            return Err(Failure::Busy);
        }
        Ok(spi)
    }

    pub fn state(&self) -> State {
        self.state
    }

    /// Observe the physical active bit of this exact SPI.
    pub fn active(&mut self) -> bool {
        self.bit(ACTIVE)
    }

    fn word(&mut self, offset: usize) -> u32 {
        u32::from_le(unsafe { self.mapping.read_u32(offset) }.expect("checked GICD window"))
    }
    fn write(&mut self, offset: usize, bits: u32) {
        unsafe { self.mapping.write_u32(offset, bits.to_le()) }.expect("checked GICD window");
    }
    fn bank(&self, offset: usize) -> usize {
        offset + self.route.intid / 32 * 4
    }
    fn mask(&self) -> u32 {
        1 << (self.route.intid % 32)
    }
    fn bit(&mut self, offset: usize) -> bool {
        self.word(self.bank(offset)) & self.mask() != 0
    }

    /// # Safety
    /// The supplied affinity identifies a present target PE in this
    /// domain whose Group 1 reception will be available before unmasking.
    /// The source remains quiescent throughout the register updates and the
    /// caller has settled any previous binding and its in-flight receptions.
    pub unsafe fn configure(&mut self, affinity: u64, priority: u8) -> Result<(), Failure> {
        let operation = State::Configuring { affinity, priority };
        if self.state != State::Masked && self.state != operation {
            return Err(Failure::State);
        }
        if affinity & !AFFINITY_MASK != 0 {
            return Err(Failure::Profile);
        }
        if self.bit(ENABLE) || self.bit(ACTIVE) || self.bit(PENDING) {
            return Err(Failure::Busy);
        }
        self.state = operation;
        if self.word(CTLR) & SINGLE_SECURITY_STATE != 0 {
            let group = self.bank(GROUP);
            let value = self.word(group) | self.mask();
            self.write(group, value);
            if !self.bit(GROUP) {
                return Err(Failure::Unconfirmed);
            }
        }
        let id = self.route.intid;
        let config = CONFIG + (id / 16) * 4;
        let edge = 2 << ((id % 16) * 2);
        let wanted = if self.route.trigger == Trigger::EdgeRising {
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
        (self.barrier.0)();
        if self.word(config) & edge != wanted
            || unsafe { self.mapping.read_u8(PRIORITY + id) }.expect("checked GICD priority")
                != priority
            || u64::from_le(
                unsafe { self.mapping.read_u64(ROUTER + id * 8) }.expect("checked GICD route"),
            ) != affinity
        {
            return Err(Failure::Unconfirmed);
        }
        self.state = State::Configured;
        Ok(())
    }

    /// # Safety
    /// The target can receive this binding, including a pending assertion,
    /// and the software owner can retain the original claim and EOI.
    pub unsafe fn enable(&mut self) -> Result<(), Failure> {
        if self.state != State::Configured {
            return Err(Failure::State);
        }
        self.state = State::Enabling;
        self.write(self.bank(ENABLE), self.mask());
        (self.barrier.0)();
        if !self.bit(ENABLE) {
            return Err(Failure::Unconfirmed);
        }
        self.state = State::Enabled;
        Ok(())
    }

    /// Disable this source. Repeat when the Distributor still reports RWP.
    /// A successful mask does not retire pending or active receptions.
    pub fn disable(&mut self) -> Result<(), Failure> {
        if matches!(self.state, State::Configuring { .. }) {
            return Err(Failure::Unconfirmed);
        }
        if self.state == State::Masked {
            return Ok(());
        }
        if self.state != State::Masking {
            self.state = State::Masking;
            self.write(self.bank(DISABLE), self.mask());
            (self.barrier.0)();
        }
        if self.word(CTLR) & WRITE_PENDING != 0 || self.bit(ENABLE) {
            return Err(Failure::Busy);
        }
        self.state = State::Masked;
        Ok(())
    }
}
