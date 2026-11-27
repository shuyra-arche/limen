//! Non-secure command queue for configurations with a linear stream table,
//! stage-2 translation and no endpoint translation caches.

use super::{ABORT, CR0, CR0_ACK, DeviceWindow, Error, GBPA, UPDATE, read};
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
    Failed,
}

pub struct Queue<'a> {
    window: &'a mut DeviceWindow,
    ring: &'static mut Ring,
    order: fn(),
    sid_bits: u32,
    producer: u32,
    state: State,
}

impl<'a> Queue<'a> {
    /// Configure a fresh command queue only on a fully disabled SMMU with
    /// GBPA.ABORT acknowledged and no previous producer to resume.
    ///
    /// # Safety
    /// Control the Non-secure interface, including reset and all software
    /// producers. Retain the old stream/translation state for its own
    /// obligations. The naturally aligned ring must reside at `physical`
    /// in pinned device-visible, coherent memory with CR1-compatible cache
    /// attributes. Supply device and coherent-memory ordering in `order`.
    /// Retain the ring and hardware obligations even if the observer is lost.
    pub unsafe fn open(
        window: &'a mut DeviceWindow,
        ring: &'static mut Ring,
        physical: PhysicalAddress,
        order: fn(),
    ) -> Result<Self, Error> {
        let idr1 = unsafe { read(window, 4)? };
        let address = physical.get();
        if address & 63 != 0 || address >> 52 != 0 {
            return Err(Error::Window);
        }
        if idr1 & (1 << 29) != 0 || ((idr1 >> 21) & 31) < LOG_ENTRIES {
            return Err(Error::Rejected);
        }
        if unsafe { read(window, CR0)? } != 0 || unsafe { read(window, CR0_ACK)? } != 0 {
            return Err(Error::Active);
        }
        if unsafe { read(window, GBPA)? } & (ABORT | UPDATE) != ABORT {
            return Err(Error::Rejected);
        }
        if !window.supports_u32(CONSUMER) {
            return Err(Error::Window);
        }
        let base = address | u64::from(LOG_ENTRIES);
        unsafe { window.write_u64(BASE, base.to_le()) }.map_err(|_| Error::Window)?;
        order();
        if u64::from_le(unsafe { window.read_u64(BASE) }.map_err(|_| Error::Window)?)
            & ((1 << 52) - 1)
            != base
        {
            return Err(Error::Rejected);
        }
        unsafe { window.write_u32(PRODUCER, 0) }.map_err(|_| Error::Window)?;
        unsafe { window.write_u32(CONSUMER, 0) }.map_err(|_| Error::Window)?;
        order();
        unsafe { window.write_u32(CR0, QUEUE_ENABLE.to_le()) }.map_err(|_| Error::Window)?;
        Ok(Self {
            window,
            ring,
            order,
            sid_bits: idr1 & 63,
            producer: 0,
            state: State::Starting,
        })
    }

    fn healthy(&mut self) -> Result<u32, Error> {
        (self.order)();
        let errors =
            unsafe { read(self.window, GLOBAL_ERROR)? ^ read(self.window, GLOBAL_ERROR_ACK)? };
        let consumer = unsafe { read(self.window, CONSUMER)? };
        // Exclusive control: never acknowledge, skip a bad command or recover
        // by resetting the ring while this operation is outstanding.
        if errors != 0 || consumer & !POINTER_MASK != 0 {
            self.state = State::Failed;
            return Err(Error::Rejected);
        }
        if unsafe { read(self.window, CR0_ACK)? } & QUEUE_ENABLE == 0 {
            return Err(Error::Busy);
        }
        Ok(consumer)
    }

    /// Reserve two adjacent positions, publish the scoped command followed by
    /// CMD_SYNC without an MSI, then ring the producer doorbell only once.
    pub fn submit(&mut self, operation: Invalidate) -> Result<(), Error> {
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
        let consumer = self.healthy()?;
        if consumer != self.producer || unsafe { read(self.window, PRODUCER)? } != self.producer {
            self.state = State::Failed;
            return Err(Error::Rejected);
        }
        let slot = (self.producer % ENTRIES) as usize;
        self.ring.0[slot] = [word.to_le(), 0];
        self.ring.0[(slot + 1) % ENTRIES as usize] = [0x46u64.to_le(), 0];
        (self.order)();
        self.producer = (self.producer + 2) & POINTER_MASK;
        self.state = State::Issued(operation);
        unsafe { self.window.write_u32(PRODUCER, self.producer.to_le()) }.map_err(|_| Error::Window)
    }

    /// One exclusive producer and one outstanding batch disambiguate wrap.
    /// Only consumption through this batch's CMD_SYNC completes the operation.
    pub fn observe(&mut self) -> Result<Invalidate, Error> {
        let State::Issued(operation) = self.state else {
            return Err(Error::Active);
        };
        if self.healthy()? != self.producer {
            return Err(Error::Busy);
        }
        self.healthy()?;
        self.state = State::Ready;
        Ok(operation)
    }

    pub fn shut(&mut self) -> Result<(), Error> {
        if matches!(self.state, State::Starting | State::Ready) {
            self.healthy()?;
            self.state = State::Shutting;
            unsafe { self.window.write_u32(CR0, 0) }.map_err(|_| Error::Window)?;
        }
        if self.state != State::Shutting {
            return Err(Error::Active);
        }
        (self.order)();
        if unsafe { read(self.window, CR0_ACK)? } != 0 {
            return Err(Error::Busy);
        }
        self.state = State::Shut;
        Ok(())
    }
}
