//! Arm SMMUv3 non-secure stage-two control for one physical interface.

use crate::metal::{
    memory::mmu::access::device::DeviceWindow,
    platform::description::devicetree::{Error as TreeError, Node},
};
use limen_treaty::model::{PhysicalAddress, PhysicalRange};
use queue::Queue;

const IDR0: usize = 0;
const IDR1: usize = 4;
const IDR5: usize = 0x14;
const CR0: usize = 0x20;
const CR0_ACK: usize = 0x24;
const GBPA: usize = 0x44;
const STREAM_TABLE: usize = 0x80;
const STREAM_CONFIG: usize = 0x88;
const SMMU_ENABLE: u32 = 1;
const ABORT: u32 = 1 << 20;
const UPDATE: u32 = 1 << 31;

pub fn registers(node: Node<'_, '_>) -> Result<Option<PhysicalRange>, TreeError> {
    if !node.enabled()? || !node.compatible(b"arm,smmu-v3")? {
        return Ok(None);
    }
    if node.property(b"#iommu-cells")? != Some(&1u32.to_be_bytes()) {
        return Err(TreeError::Unsupported);
    }
    Ok(Some(node.register(0)?))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Window,
    Busy,
    Active,
    Rejected,
    Unconfirmed,
}

#[derive(Clone, Copy, Debug)]
pub struct Profile {
    pub stage_two: bool,
    pub granule_4k: bool,
    pub enabled: bool,
    aarch64_tables: bool,
}

/// Device and coherent-memory ordering supplied by the native processing element.
pub struct DeviceOrder(fn());

impl DeviceOrder {
    #[cfg(all(limen_target, target_arch = "aarch64"))]
    pub fn native() -> Self {
        Self(|| unsafe { core::arch::asm!("dsb sy", options(nostack, preserves_flags)) })
    }

    fn fence(&self) {
        (self.0)();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamTable {
    pub base: PhysicalAddress,
    pub bits: u8,
}

#[derive(Clone, Copy)]
enum State {
    Disabled,
    Aborting,
    Blocked,
    Installing(StreamTable),
    Enabled(StreamTable),
    Unconfirmed(Option<StreamTable>),
}

/// The disabled policy, stream directory and command queue retain the same
/// exclusive register authority through every transition.
#[must_use]
pub struct Controller<'a> {
    window: &'a mut DeviceWindow,
    order: DeviceOrder,
    profile: Profile,
    state: State,
    queue: Option<Queue>,
}

impl<'a> Controller<'a> {
    /// # Safety
    /// Map the actual Non-secure, little-endian register file, exclude other
    /// software producers and reset, and control all covered streams. Transfer
    /// only a disabled interface with no pending command or old transactions.
    /// Retain hardware-visible resources through uncertain effects and loss of
    /// this controller.
    pub unsafe fn admit(window: &'a mut DeviceWindow, order: DeviceOrder) -> Result<Self, Error> {
        let idr0 = unsafe { read(window, IDR0)? };
        let idr5 = unsafe { read(window, IDR5)? };
        let requested = unsafe { read(window, CR0)? };
        let observed = unsafe { read(window, CR0_ACK)? };
        if requested != observed {
            return Err(Error::Busy);
        }
        if observed != 0 {
            return Err(Error::Active);
        }
        Ok(Self {
            window,
            order,
            profile: Profile {
                stage_two: idr0 & 1 != 0,
                granule_4k: idr5 & (1 << 4) != 0,
                enabled: false,
                aarch64_tables: (idr0 >> 2) & 2 != 0,
            },
            state: State::Disabled,
            queue: None,
        })
    }

    pub fn profile(&self) -> Profile {
        self.profile
    }

    pub fn format(&self) -> Result<translation::Format, Error> {
        translation::Format::new(self.profile)
    }

    /// The selected or potentially selected stream table remains retained.
    pub fn retained_table(&self) -> Option<StreamTable> {
        match self.state {
            State::Installing(table) | State::Enabled(table) => Some(table),
            State::Unconfirmed(table) => table,
            _ => None,
        }
    }

    /// # Safety
    /// Control the policy for all streams and retain any prior device
    /// transactions and translation obligations independently.
    pub unsafe fn begin_abort_bypass(&mut self) -> Result<(), Error> {
        match self.state {
            State::Disabled => {}
            State::Unconfirmed(_) => return Err(Error::Unconfirmed),
            _ => return Err(Error::Active),
        }
        if unsafe { read(self.window, GBPA)? } & UPDATE != 0 {
            return Err(Error::Busy);
        }
        self.state = State::Aborting;
        self.order.fence();
        unsafe { self.window.write_u32(GBPA, (UPDATE | ABORT).to_le()) }.map_err(|_| Error::Window)
    }

    /// Confirms GBPA.ABORT while the interface is disabled.
    pub fn observe_abort(&mut self) -> Result<(), Error> {
        match self.state {
            State::Aborting => {}
            State::Unconfirmed(_) => return Err(Error::Unconfirmed),
            _ => return Err(Error::Active),
        }
        self.order.fence();
        let policy = unsafe { read(self.window, GBPA)? };
        if policy & UPDATE != 0 {
            return Err(Error::Busy);
        }
        if policy & ABORT == 0
            || unsafe { read(self.window, CR0)? | read(self.window, CR0_ACK)? } != 0
        {
            self.state = State::Unconfirmed(None);
            return Err(Error::Unconfirmed);
        }
        self.state = State::Blocked;
        Ok(())
    }

    /// Select a linear stream table from the blocked and disabled interface.
    ///
    /// # Safety
    /// Keep the complete aligned table (2^bits 64-byte entries) and all
    /// descendants visible in pinned, coherent device memory. Account for
    /// every route and alias and keep requesters stopped until independent
    /// configuration, translation and endpoint obligations settle.
    pub unsafe fn begin_install(&mut self, base: PhysicalAddress, bits: u8) -> Result<(), Error> {
        match self.state {
            State::Blocked if self.queue.is_none() => {}
            State::Unconfirmed(_) => return Err(Error::Unconfirmed),
            _ => return Err(Error::Active),
        }
        let idr1 = unsafe { read(self.window, IDR1)? };
        if idr1 & (1 << 30) != 0 || u32::from(bits) > (idr1 & 63) || bits > 31 {
            return Err(Error::Rejected);
        }
        let address = base.get();
        if !address.is_multiple_of(64u64 << bits) || address >> 52 != 0 {
            return Err(Error::Window);
        }
        if !self.window.supports_u64(STREAM_TABLE) || !self.window.supports_u32(STREAM_CONFIG) {
            return Err(Error::Window);
        }
        let table = StreamTable { base, bits };
        self.state = State::Installing(table);
        self.order.fence();
        unsafe { self.window.write_u64(STREAM_TABLE, address.to_le()) }
            .map_err(|_| Error::Window)?;
        unsafe {
            self.window
                .write_u32(STREAM_CONFIG, u32::from(bits).to_le())
        }
        .map_err(|_| Error::Window)?;
        self.order.fence();
        if unsafe { read(self.window, STREAM_CONFIG)? } & 0x3ffff != u32::from(bits)
            || u64::from_le(
                unsafe { self.window.read_u64(STREAM_TABLE) }.map_err(|_| Error::Window)?,
            ) & ((1 << 52) - 1)
                != address
        {
            self.state = State::Unconfirmed(Some(table));
            return Err(Error::Unconfirmed);
        }
        unsafe { self.window.write_u32(CR0, SMMU_ENABLE.to_le()) }.map_err(|_| Error::Window)?;
        Ok(())
    }

    pub fn observe_install(&mut self) -> Result<(), Error> {
        let table = match self.state {
            State::Installing(table) => table,
            State::Unconfirmed(_) => return Err(Error::Unconfirmed),
            _ => return Err(Error::Active),
        };
        self.order.fence();
        if unsafe { read(self.window, CR0)? } != SMMU_ENABLE {
            self.state = State::Unconfirmed(Some(table));
            return Err(Error::Unconfirmed);
        }
        if unsafe { read(self.window, CR0_ACK)? } != SMMU_ENABLE {
            return Err(Error::Busy);
        }
        self.profile.enabled = true;
        self.state = State::Enabled(table);
        Ok(())
    }

    /// # Safety
    /// The ring must be exclusive, physically contiguous at `physical`, coherent
    /// and retained until its original hardware use has ended.
    pub unsafe fn open_queue(
        &mut self,
        ring: &'static mut queue::Ring,
        physical: PhysicalAddress,
    ) -> Result<(), Error> {
        match self.state {
            State::Blocked | State::Enabled(_) if self.queue.is_none() => {}
            State::Unconfirmed(_) => return Err(Error::Unconfirmed),
            _ => return Err(Error::Active),
        }
        let queue = unsafe { Queue::prepare(self.window, ring, physical) }?;
        self.queue.insert(queue).start(self.window, &self.order)
    }

    /// The original queue storage, including an unconfirmed installation.
    pub fn retained_queue(&self) -> Option<PhysicalAddress> {
        self.queue.as_ref().map(Queue::physical)
    }

    pub fn submit(&mut self, operation: queue::Invalidate) -> Result<(), Error> {
        self.queue
            .as_mut()
            .ok_or(Error::Active)?
            .submit(self.window, &self.order, operation)
    }

    pub fn observe(&mut self) -> Result<queue::Invalidate, Error> {
        self.queue
            .as_mut()
            .ok_or(Error::Active)?
            .observe(self.window, &self.order)
    }

    /// The exact outstanding operation, including after queue faults.
    pub fn unsettled_invalidation(&self) -> Option<queue::Invalidate> {
        self.queue.as_ref().and_then(Queue::unsettled)
    }

    pub fn shut_queue(&mut self) -> Result<(), Error> {
        self.queue
            .as_mut()
            .ok_or(Error::Active)?
            .shut(self.window, &self.order)?;
        self.queue = None;
        Ok(())
    }
}

unsafe fn read(window: &mut DeviceWindow, offset: usize) -> Result<u32, Error> {
    unsafe { window.read_u32(offset) }
        .map(u32::from_le)
        .map_err(|_| Error::Window)
}

pub mod queue;
pub mod translation;

#[cfg(test)]
#[path = "../../../../../../../tests/metal/memory/mmu/aarch64/external/smmu/mod.rs"]
mod tests;
