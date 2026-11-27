//! One exclusive command queue with one outstanding invalidation and its fence.

use super::{DeviceOrder, DeviceWindow, Error, PhysicalAddress};

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
pub(super) enum Invalidate {
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
    Fault(Option<Invalidate>),
}

impl State {
    fn admits_command(self) -> Result<(), Error> {
        match self {
            Self::Starting | Self::Idle => Ok(()),
            Self::InFlight(_) | Self::Shutting => Err(Error::Busy),
            Self::Fault(_) => Err(Error::Unconfirmed),
            Self::Shut => Err(Error::Mode),
        }
    }
}

/// Once opened the page is permanent, including across queue faults. The
/// sole producer can consume only the in-flight operation's own fence.
pub(super) struct Queue {
    page: &'static mut Page,
    physical: PhysicalAddress,
    generation: u64,
    tail: u32,
    state: State,
}

impl Queue {
    /// # Safety
    /// The entire physical page must name `page` in a pinned, coherent mapping
    /// for this IOMMU. It must be exclusively owned, in scope for its reported
    /// physical address width, and accessible by the controller until shutdown.
    /// Own a little-endian IOMMU and its disabled command queue; no independent
    /// producer, reset, enable or error clear is allowed for this generation.
    /// Keep original controls, queue storage and unsettled translations retained
    /// if the observer is lost or the controller fails.
    pub(super) unsafe fn prepare(
        window: &mut DeviceWindow<'_>,
        page: &'static mut Page,
        physical: PhysicalAddress,
        generation: u64,
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
        if status & ERRORS != 0 {
            return Err(Error::Rejected);
        }
        if !window.supports_u64(BASE) || !window.supports_u32(TAIL) {
            return Err(Error::Window);
        }
        Ok(Self {
            page,
            physical,
            generation,
            tail: 0,
            state: State::Starting,
        })
    }

    pub(super) fn open_generation(&self) -> Result<u64, Error> {
        self.state.admits_command()?;
        Ok(self.generation)
    }

    pub(super) fn physical(&self) -> PhysicalAddress {
        self.physical
    }

    /// The controller retains this queue before the first write. Inexact
    /// readback blocks further admission while that storage remains held.
    pub(super) fn start(
        &mut self,
        window: &mut DeviceWindow<'_>,
        order: &DeviceOrder,
    ) -> Result<(), Error> {
        let base = (self.physical.get() >> 2) | 7;
        unsafe { window.write_u64(BASE, base.to_le()) }.map_err(|_| Error::Window)?;
        order.fence();
        let actual = u64::from_le(unsafe { window.read_u64(BASE) }.map_err(|_| Error::Window)?);
        if actual != base {
            return Err(self.fail());
        }
        unsafe { window.write_u32(TAIL, 0) }.map_err(|_| Error::Window)?;
        order.fence();
        unsafe { window.write_u32(STATUS, ENABLE.to_le()) }.map_err(|_| Error::Window)
    }

    pub(super) fn into_storage(self) -> (&'static mut Page, PhysicalAddress) {
        assert_eq!(self.state, State::Shut);
        (self.page, self.physical)
    }

    pub(super) fn unsettled(&self) -> Option<Invalidate> {
        match self.state {
            State::InFlight(operation) | State::Fault(Some(operation)) => Some(operation),
            _ => None,
        }
    }

    fn fail(&mut self) -> Error {
        self.state = State::Fault(self.unsettled());
        Error::Rejected
    }

    fn read(window: &mut DeviceWindow<'_>, offset: usize) -> Result<u32, Error> {
        unsafe { window.read_u32(offset) }
            .map(u32::from_le)
            .map_err(|_| Error::Window)
    }

    fn active(&mut self, window: &mut DeviceWindow<'_>, order: &DeviceOrder) -> Result<(), Error> {
        order.fence();
        let status = Self::read(window, STATUS)?;
        if status & ERRORS != 0 || status & ENABLE == 0 {
            return Err(self.fail());
        }
        if status & (ACTIVE | BUSY) != ACTIVE {
            return Err(Error::Busy);
        }
        Ok(())
    }

    /// Prepare and publish the invalidation followed by IOFENCE.C(PR,PW).
    /// The caller first makes the directory or PTE change visible and retains
    /// all affected resources. No new batch is admitted until observation.
    pub(super) fn submit(
        &mut self,
        window: &mut DeviceWindow<'_>,
        order: &DeviceOrder,
        invalidate: Invalidate,
    ) -> Result<(), Error> {
        let word = match invalidate {
            Invalidate::Device(did) if did < 1 << 24 => 3 | (1 << 33) | (u64::from(did) << 40),
            Invalidate::Device(_) => return Err(Error::Address),
            Invalidate::FirstStage => 1,
        };
        self.state.admits_command()?;
        self.active(window, order)?;
        if Self::read(window, HEAD)? != self.tail || Self::read(window, TAIL)? != self.tail {
            return Err(self.fail());
        }
        let index = self.tail;
        let fence = (index + 1) % ENTRIES;
        let next = (fence + 1) % ENTRIES;
        self.page.0[index as usize] = [word.to_le(), 0];
        self.page.0[fence as usize] = [IOFENCE_READ_WRITE.to_le(), 0];
        order.fence();
        self.state = State::InFlight(invalidate);
        self.tail = next;
        unsafe { window.write_u32(TAIL, next.to_le()) }.map_err(|_| Error::Window)
    }

    /// Returns the exact completed operation. One outstanding batch excludes
    /// ring ambiguity, even at wrap. No cache or endpoint beyond the scope of
    /// the invalidation and PR/PW is retired by this observation.
    pub(super) fn observe(
        &mut self,
        window: &mut DeviceWindow<'_>,
        order: &DeviceOrder,
    ) -> Result<Invalidate, Error> {
        let operation = match self.state {
            State::InFlight(operation) => operation,
            State::Fault(Some(_)) => return Err(Error::Unconfirmed),
            _ => return Err(Error::Mode),
        };
        self.active(window, order)?;
        let head = Self::read(window, HEAD)?;
        // Read errors after the head sample so an errored batch cannot appear
        // to have completed on the strength of an earlier healthy snapshot.
        self.active(window, order)?;
        if head != self.tail {
            return Err(Error::Busy);
        }
        self.state = State::Idle;
        Ok(operation)
    }

    pub(super) fn shut(
        &mut self,
        window: &mut DeviceWindow<'_>,
        order: &DeviceOrder,
    ) -> Result<(), Error> {
        if matches!(self.state, State::Starting | State::Idle) {
            self.active(window, order)?;
            self.state = State::Shutting;
            unsafe { window.write_u32(STATUS, 0) }.map_err(|_| Error::Window)?;
        }
        if self.state != State::Shutting {
            return Err(Error::Mode);
        }
        order.fence();
        let status = Self::read(window, STATUS)?;
        if status & ERRORS != 0 {
            return Err(self.fail());
        }
        if status & (BUSY | ENABLE | ACTIVE) != 0 {
            return Err(Error::Busy);
        }
        self.state = State::Shut;
        Ok(())
    }
}

#[cfg(test)]
#[path = "../../../../../../../tests/metal/memory/mmu/riscv64/external/iommu/queue.rs"]
mod tests;
