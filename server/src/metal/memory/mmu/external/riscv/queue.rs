//! One exclusive command queue with one outstanding invalidation and its fence.

use super::{DeviceWindow, Error, PhysicalAddress};

const BASE: usize = 0x18;
const HEAD: usize = 0x20;
const TAIL: usize = 0x24;
const STATUS: usize = 0x48;
const ENABLE: u32 = 1;
const ACTIVE: u32 = 1 << 16;
const BUSY: u32 = 1 << 17;
const ERRORS: u32 = (1 << 8) | (1 << 9) | (1 << 10);
const ENTRIES: u32 = 256;
const IOFENCE_READ_WRITE: u64 = 2 | (1 << 12) | (1 << 13);

/// A single 4 KiB page of little-endian 16-byte commands.
#[repr(C, align(4096))]
pub struct Page([[u64; 2]; ENTRIES as usize]);
impl Page {
    pub const EMPTY: Self = Self([[0; 2]; ENTRIES as usize]);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Invalidate {
    Device(u32),
    FirstStage,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Starting,
    Idle,
    InFlight(Invalidate),
    Shutting,
    Shut,
    Fault,
}

/// Once opened the page is permanent, including across queue faults. The
/// sole producer can consume only the in-flight operation's own fence.
pub struct Queue<'a> {
    window: &'a mut DeviceWindow,
    page: &'static mut Page,
    order: fn(),
    tail: u32,
    state: State,
}

impl<'a> Queue<'a> {
    /// # Safety
    /// The entire physical page must name `page` in a pinned, coherent mapping
    /// for this IOMMU. It must be exclusively owned, in scope for its reported
    /// physical address width, and accessible by the controller until shutdown.
    /// Own a little-endian IOMMU and its disabled command queue; no independent
    /// producer, reset, enable or error clear is allowed for this generation.
    /// `order` must order coherent command memory and I/O in both directions.
    /// Keep original controls, queue storage and unsettled translations retained
    /// if the observer is lost or the controller fails.
    pub unsafe fn open(
        window: &'a mut DeviceWindow,
        page: &'static mut Page,
        physical: PhysicalAddress,
        order: fn(),
    ) -> Result<Self, Error> {
        let address = physical.get();
        if !address.is_multiple_of(4096) || address >> 56 != 0 {
            return Err(Error::Address);
        }
        let status = unsafe { window.read_u32(STATUS) }
            .map(u32::from_le)
            .map_err(|_| Error::Window)?;
        if status & (ENABLE | ACTIVE | BUSY) != 0 {
            return Err(Error::Busy);
        }
        // The maximum offset was already checked by the status read. Validate
        // the 64-bit access before changing control or enabling DMA.
        let base = (address >> 2) | 7;
        unsafe { window.write_u64(BASE, base.to_le()) }.map_err(|_| Error::Window)?;
        order();
        if unsafe { window.read_u64(BASE) }
            .map(u64::from_le)
            .map_err(|_| Error::Window)?
            != base
        {
            return Err(Error::Rejected);
        }
        unsafe { window.write_u32(TAIL, 0) }.map_err(|_| Error::Window)?;
        order();
        unsafe { window.write_u32(STATUS, ENABLE.to_le()) }.map_err(|_| Error::Window)?;
        Ok(Self {
            window,
            page,
            tail: 0,
            order,
            state: State::Starting,
        })
    }

    fn read(&mut self, offset: usize) -> Result<u32, Error> {
        unsafe { self.window.read_u32(offset) }
            .map(u32::from_le)
            .map_err(|_| Error::Window)
    }

    fn active(&mut self) -> Result<(), Error> {
        (self.order)();
        let status = self.read(STATUS)?;
        if status & ERRORS != 0 || status & ENABLE == 0 {
            self.state = State::Fault;
            return Err(Error::Rejected);
        }
        if status & (ACTIVE | BUSY) != ACTIVE {
            return Err(Error::Busy);
        }
        Ok(())
    }

    /// Prepare and publish the invalidation followed by IOFENCE.C(PR,PW).
    /// The caller first makes the directory or PTE change visible and retains
    /// all affected resources. No new batch is admitted until observation.
    pub fn submit(&mut self, invalidate: Invalidate) -> Result<(), Error> {
        let word = match invalidate {
            Invalidate::Device(did) if did < 1 << 24 => 3 | (1 << 33) | (u64::from(did) << 40),
            Invalidate::Device(_) => return Err(Error::Address),
            Invalidate::FirstStage => 1,
        };
        if !matches!(self.state, State::Starting | State::Idle) {
            return Err(Error::Mode);
        }
        self.active()?;
        if self.read(HEAD)? != self.tail || self.read(TAIL)? != self.tail {
            self.state = State::Fault;
            return Err(Error::Rejected);
        }
        let index = self.tail;
        let fence = (index + 1) % ENTRIES;
        let next = (fence + 1) % ENTRIES;
        self.page.0[index as usize] = [word.to_le(), 0];
        self.page.0[fence as usize] = [IOFENCE_READ_WRITE.to_le(), 0];
        (self.order)();
        self.state = State::InFlight(invalidate);
        self.tail = next;
        unsafe { self.window.write_u32(TAIL, next.to_le()) }.map_err(|_| Error::Window)
    }

    /// Returns the exact completed operation. One outstanding batch excludes
    /// ring ambiguity, even at wrap. No cache or endpoint beyond the scope of
    /// the invalidation and PR/PW is retired by this observation.
    pub fn observe(&mut self) -> Result<Invalidate, Error> {
        let State::InFlight(operation) = self.state else {
            return Err(Error::Mode);
        };
        self.active()?;
        let head = self.read(HEAD)?;
        // Read errors after the head sample so an errored batch cannot appear
        // to have completed on the strength of an earlier healthy snapshot.
        self.active()?;
        if head != self.tail {
            return Err(Error::Busy);
        }
        self.state = State::Idle;
        Ok(operation)
    }

    pub fn shut(&mut self) -> Result<(), Error> {
        if self.state == State::Idle {
            self.active()?;
            self.state = State::Shutting;
            unsafe { self.window.write_u32(STATUS, 0) }.map_err(|_| Error::Window)?;
        }
        if self.state != State::Shutting {
            return Err(Error::Mode);
        }
        (self.order)();
        let status = self.read(STATUS)?;
        if status & ERRORS != 0 {
            self.state = State::Fault;
            return Err(Error::Rejected);
        }
        if status & (BUSY | ENABLE | ACTIVE) != 0 {
            return Err(Error::Busy);
        }
        self.state = State::Shut;
        Ok(())
    }
}
