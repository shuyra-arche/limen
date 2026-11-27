//! Non-secure command queue for configurations with a linear stream table,
//! stage-2 translation and no endpoint translation caches.

use super::{ABORT, CR0, CR0_ACK, DeviceOrder, DeviceWindow, Error, GBPA, UPDATE, read};
use limen_treaty::model::PhysicalAddress;

const BASE: usize = 0x90;
const PRODUCER: usize = 0x98;
const CONSUMER: usize = 0x9c;
const GLOBAL_ERROR: usize = 0x60;
const GLOBAL_ERROR_ACK: usize = 0x64;
const QUEUE_ENABLE: u32 = 1 << 3;
const LOG_ENTRIES: u32 = 2;
const ENTRIES: u32 = 1 << LOG_ENTRIES;
const POINTER_MASK: u32 = (ENTRIES << 1) - 1;

#[repr(C, align(64))]
pub struct Ring([[u64; 2]; ENTRIES as usize]);
impl Ring {
    pub const EMPTY: Self = Self([[0; 2]; ENTRIES as usize]);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Invalidate {
    Stream(u32),
    StageTwo(u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Starting,
    Ready,
    Issued(Invalidate),
    Shutting,
    Shut,
    Failed(Option<Invalidate>),
}

pub(super) struct Queue {
    ring: &'static mut Ring,
    physical: PhysicalAddress,
    retained_control: u32,
    sid_bits: u32,
    producer: u32,
    state: State,
}

impl Queue {
    /// Configure a fresh command queue on a stopped queue with
    /// GBPA.ABORT acknowledged and no previous producer to resume.
    ///
    /// # Safety
    /// Control the Non-secure interface, including reset and all software
    /// producers. Retain the old stream/translation state for its own
    /// obligations. The naturally aligned ring must reside at `physical`
    /// in pinned device-visible, coherent memory with CR1-compatible cache
    /// attributes.
    /// Retain the ring and hardware obligations even if the observer is lost.
    pub(super) unsafe fn prepare(
        window: &mut DeviceWindow,
        ring: &'static mut Ring,
        physical: PhysicalAddress,
    ) -> Result<Self, Error> {
        let idr1 = unsafe { read(window, 4)? };
        let address = physical.get();
        if address & 63 != 0 || address >> 52 != 0 {
            return Err(Error::Window);
        }
        if idr1 & (1 << 29) != 0 || ((idr1 >> 21) & 31) < LOG_ENTRIES {
            return Err(Error::Rejected);
        }
        let retained_control = unsafe { read(window, CR0)? };
        if retained_control & !super::SMMU_ENABLE != 0
            || unsafe { read(window, CR0_ACK)? } != retained_control
        {
            return Err(Error::Active);
        }
        if unsafe { read(window, GBPA)? } & (ABORT | UPDATE) != ABORT {
            return Err(Error::Rejected);
        }
        if !window.supports_u64(BASE)
            || !window.supports_u32(PRODUCER)
            || !window.supports_u32(CONSUMER)
        {
            return Err(Error::Window);
        }
        Ok(Self {
            ring,
            physical,
            retained_control,
            sid_bits: idr1 & 63,
            producer: 0,
            state: State::Starting,
        })
    }

    pub(super) fn physical(&self) -> PhysicalAddress {
        self.physical
    }

    /// The controller retains this queue before the first write. Inexact
    /// readback blocks further admission while that storage remains held.
    pub(super) fn start(
        &mut self,
        window: &mut DeviceWindow,
        order: &DeviceOrder,
    ) -> Result<(), Error> {
        let base = self.physical.get() | u64::from(LOG_ENTRIES);
        unsafe { window.write_u64(BASE, base.to_le()) }.map_err(|_| Error::Window)?;
        order.fence();
        let actual = u64::from_le(unsafe { window.read_u64(BASE) }.map_err(|_| Error::Window)?);
        if actual & ((1 << 52) - 1) != base {
            return Err(self.fail());
        }
        unsafe { window.write_u32(PRODUCER, 0) }.map_err(|_| Error::Window)?;
        unsafe { window.write_u32(CONSUMER, 0) }.map_err(|_| Error::Window)?;
        order.fence();
        unsafe { window.write_u32(CR0, (self.retained_control | QUEUE_ENABLE).to_le()) }
            .map_err(|_| Error::Window)
    }

    pub(super) fn unsettled(&self) -> Option<Invalidate> {
        match self.state {
            State::Issued(operation) | State::Failed(Some(operation)) => Some(operation),
            _ => None,
        }
    }

    fn fail(&mut self) -> Error {
        self.state = State::Failed(self.unsettled());
        Error::Rejected
    }

    fn healthy(&mut self, window: &mut DeviceWindow, order: &DeviceOrder) -> Result<u32, Error> {
        order.fence();
        let errors = unsafe { read(window, GLOBAL_ERROR)? ^ read(window, GLOBAL_ERROR_ACK)? };
        let consumer = unsafe { read(window, CONSUMER)? };
        // Exclusive control: never acknowledge, skip a bad command or recover
        // by resetting the ring while this operation is outstanding.
        if errors != 0 || consumer & !POINTER_MASK != 0 {
            return Err(self.fail());
        }
        if unsafe { read(window, CR0_ACK)? } & QUEUE_ENABLE == 0 {
            return Err(Error::Busy);
        }
        Ok(consumer)
    }

    /// Reserve two adjacent positions, publish the scoped command followed by
    /// CMD_SYNC without an MSI, then ring the producer doorbell only once.
    pub(super) fn submit(
        &mut self,
        window: &mut DeviceWindow,
        order: &DeviceOrder,
        operation: Invalidate,
    ) -> Result<(), Error> {
        let word = match operation {
            Invalidate::Stream(sid) if self.sid_bits >= 32 || sid < 1 << self.sid_bits => {
                (u64::from(sid) << 32) | 3
            }
            Invalidate::Stream(_) => return Err(Error::Rejected),
            Invalidate::StageTwo(vmid) => (u64::from(vmid) << 32) | 0x28,
        };
        if !matches!(self.state, State::Starting | State::Ready) {
            return Err(Error::Active);
        }
        let consumer = self.healthy(window, order)?;
        if consumer != self.producer || unsafe { read(window, PRODUCER)? } != self.producer {
            return Err(self.fail());
        }
        let slot = (self.producer % ENTRIES) as usize;
        self.ring.0[slot] = [word.to_le(), 0];
        self.ring.0[(slot + 1) % ENTRIES as usize] = [0x46u64.to_le(), 0];
        order.fence();
        self.producer = (self.producer + 2) & POINTER_MASK;
        self.state = State::Issued(operation);
        unsafe { window.write_u32(PRODUCER, self.producer.to_le()) }.map_err(|_| Error::Window)
    }

    /// One exclusive producer and one outstanding batch disambiguate wrap.
    /// Only consumption through this batch's CMD_SYNC completes the operation.
    pub(super) fn observe(
        &mut self,
        window: &mut DeviceWindow,
        order: &DeviceOrder,
    ) -> Result<Invalidate, Error> {
        let State::Issued(operation) = self.state else {
            return Err(Error::Active);
        };
        if self.healthy(window, order)? != self.producer {
            return Err(Error::Busy);
        }
        self.healthy(window, order)?;
        self.state = State::Ready;
        Ok(operation)
    }

    pub(super) fn shut(
        &mut self,
        window: &mut DeviceWindow,
        order: &DeviceOrder,
    ) -> Result<(), Error> {
        if matches!(self.state, State::Starting | State::Ready) {
            self.healthy(window, order)?;
            self.state = State::Shutting;
            unsafe { window.write_u32(CR0, self.retained_control.to_le()) }
                .map_err(|_| Error::Window)?;
        }
        if self.state != State::Shutting {
            return Err(Error::Active);
        }
        order.fence();
        if unsafe { read(window, CR0_ACK)? } != self.retained_control {
            return Err(Error::Busy);
        }
        self.state = State::Shut;
        Ok(())
    }
}
