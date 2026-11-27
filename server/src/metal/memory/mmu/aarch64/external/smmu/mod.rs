//! Arm SMMUv3 non-secure stage-two control for one physical interface.

use crate::metal::{
    memory::access::treaty::device::DeviceWindow,
    platform::description::devicetree::{Error as TreeError, Node},
};
use binding::Bindings;
pub use binding::{Binding, Context};
use command::Commands;
pub use command::{Command, Effect, Refresh, Sequence};
pub use directory::StreamTable;
use directory::{Directory, Entry};
use limen_treaty::model::{PhysicalAddress, PhysicalRange};
use queue::Queue;

use registers::{
    ABORT, CR0, CR0_ACK, GBPA, IDR0, IDR1, IDR5, Registers, SMMU_ENABLE, STREAM_CONFIG,
    STREAM_TABLE, UPDATE,
};

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
    Capacity,
    Exhausted,
    Stale,
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

pub struct Storage<'a> {
    pub entries: &'static mut [Entry],
    pub physical: PhysicalAddress,
    pub bindings: &'a mut [Option<Binding>],
    pub sequence: &'a mut Sequence,
}

#[derive(Clone, Copy)]
enum State {
    Disabled,
    Aborting,
    Blocked,
    Installing,
    Enabled,
    Shutting,
    Stopped,
    UnconfirmedPolicy,
    UnconfirmedTable,
}

/// The disabled policy, stream directory and command queue retain the same
/// exclusive register authority through every transition.
#[must_use]
pub struct Controller<'a> {
    registers: Registers<'a>,
    bindings: Bindings<'a>,
    sequence: &'a mut Sequence,
    commands: Commands,
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
    /// this controller. The provided physical address names the exact aligned
    /// pinned stream table, compatible with the current CR1 access attributes.
    /// Preserve this physical scope's sequence across reset and re-admission.
    pub unsafe fn admit(
        window: &'a mut DeviceWindow<'_>,
        order: DeviceOrder,
        storage: Storage<'a>,
    ) -> Result<Self, Error> {
        let mut registers = unsafe { Registers::new(window) };
        let idr0 = registers.read(IDR0)?;
        let idr5 = registers.read(IDR5)?;
        let requested = registers.read(CR0)?;
        let observed = registers.read(CR0_ACK)?;
        if requested != observed {
            return Err(Error::Busy);
        }
        if observed != 0 {
            return Err(Error::Active);
        }
        let idr1 = registers.read(IDR1)?;
        if idr1 & (1 << 30) != 0 {
            return Err(Error::Rejected);
        }
        let directory = Directory::new(storage.entries, storage.physical, idr1 & 63)?;
        let bindings = Bindings::new(directory, storage.bindings)?;
        let profile = Profile {
            stage_two: idr0 & 1 != 0,
            granule_4k: idr5 & (1 << 4) != 0,
            enabled: false,
            aarch64_tables: (idr0 >> 2) & 2 != 0,
        };
        translation::Format::new(profile)?;
        storage.sequence.claim(registers.physical())?;
        Ok(Self {
            registers,
            bindings,
            sequence: storage.sequence,
            commands: Commands::new(),
            order,
            profile,
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
            State::Installing
            | State::Enabled
            | State::Shutting
            | State::Stopped
            | State::UnconfirmedTable => Some(self.bindings.table()),
            _ => None,
        }
    }

    /// # Safety
    /// Control the policy for all streams and retain any prior device
    /// transactions and translation obligations independently.
    pub unsafe fn begin_abort_bypass(&mut self) -> Result<(), Error> {
        match self.state {
            State::Disabled => {}
            State::UnconfirmedPolicy | State::UnconfirmedTable => return Err(Error::Unconfirmed),
            _ => return Err(Error::Active),
        }
        if self.registers.read(GBPA)? & UPDATE != 0 {
            return Err(Error::Busy);
        }
        self.state = State::Aborting;
        self.order.fence();
        self.registers.write(GBPA, UPDATE | ABORT)
    }

    /// Confirms GBPA.ABORT while the interface is disabled.
    pub fn observe_abort(&mut self) -> Result<(), Error> {
        match self.state {
            State::Aborting => {}
            State::UnconfirmedPolicy | State::UnconfirmedTable => return Err(Error::Unconfirmed),
            _ => return Err(Error::Active),
        }
        self.order.fence();
        let policy = self.registers.read(GBPA)?;
        if policy & UPDATE != 0 {
            return Err(Error::Busy);
        }
        if policy & ABORT == 0 || (self.registers.read(CR0)? | self.registers.read(CR0_ACK)?) != 0 {
            self.state = State::UnconfirmedPolicy;
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
    pub unsafe fn begin_install(&mut self) -> Result<(), Error> {
        match self.state {
            State::Blocked if self.queue.is_none() => {}
            State::UnconfirmedPolicy | State::UnconfirmedTable => return Err(Error::Unconfirmed),
            _ => return Err(Error::Active),
        }
        let table = self.bindings.table();
        let address = table.base.get();
        let bits = u32::from(table.bits);
        if !self.registers.supports_base(STREAM_TABLE) || !self.registers.supports(STREAM_CONFIG) {
            return Err(Error::Window);
        }
        self.state = State::Installing;
        self.order.fence();
        self.registers.write_base(STREAM_TABLE, address)?;
        self.registers.write(STREAM_CONFIG, bits)?;
        self.order.fence();
        if self.registers.read(STREAM_CONFIG)? & 0x3ffff != bits
            || self.registers.read_base(STREAM_TABLE)? & ((1 << 52) - 1) != address
        {
            self.state = State::UnconfirmedTable;
            return Err(Error::Unconfirmed);
        }
        self.registers.write(CR0, SMMU_ENABLE)
    }

    pub fn observe_install(&mut self) -> Result<(), Error> {
        match self.state {
            State::Installing => {}
            State::UnconfirmedPolicy | State::UnconfirmedTable => return Err(Error::Unconfirmed),
            _ => return Err(Error::Active),
        };
        self.order.fence();
        if self.registers.read(CR0)? != SMMU_ENABLE {
            self.state = State::UnconfirmedTable;
            return Err(Error::Unconfirmed);
        }
        if self.registers.read(CR0_ACK)? != SMMU_ENABLE {
            return Err(Error::Busy);
        }
        self.profile.enabled = true;
        self.state = State::Enabled;
        Ok(())
    }

    /// Disable the interface after all bindings and the queue have retired.
    ///
    /// # Safety
    /// Keep requesters isolated and retain any independently outstanding
    /// hardware obligation. The original table remains in custody even
    /// after the controller reports a disabled interface.
    pub unsafe fn begin_shut(&mut self) -> Result<(), Error> {
        let State::Enabled = self.state else {
            return Err(Error::Active);
        };
        if self.queue.is_some() || self.commands.command().is_some() || self.bindings.occupied() {
            return Err(Error::Active);
        }
        self.state = State::Shutting;
        self.order.fence();
        self.registers.write(CR0, 0)
    }

    pub fn observe_shut(&mut self) -> Result<(), Error> {
        match self.state {
            State::Shutting => {}
            State::UnconfirmedPolicy | State::UnconfirmedTable => return Err(Error::Unconfirmed),
            _ => return Err(Error::Active),
        };
        self.order.fence();
        if self.registers.read(CR0)? != 0 || self.registers.read(GBPA)? & (UPDATE | ABORT) != ABORT
        {
            self.state = State::UnconfirmedTable;
            return Err(Error::Unconfirmed);
        }
        if self.registers.read(CR0_ACK)? != 0 {
            return Err(Error::Busy);
        }
        self.profile.enabled = false;
        self.state = State::Stopped;
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
            State::Enabled if self.queue.is_none() => {}
            State::UnconfirmedPolicy | State::UnconfirmedTable => return Err(Error::Unconfirmed),
            _ => return Err(Error::Active),
        }
        let generation = self.sequence.next_available()?;
        let queue = Queue::prepare(&mut self.registers, ring, physical, generation)?;
        self.sequence.next()?;
        self.queue
            .insert(queue)
            .start(&mut self.registers, &self.order)
    }

    /// The original queue storage, including an unconfirmed installation.
    pub fn retained_queue(&self) -> Option<PhysicalAddress> {
        self.queue.as_ref().map(Queue::physical)
    }

    /// The exact outstanding binding operation, retained across queue faults.
    pub fn unsettled_effect(&self) -> Option<Effect> {
        self.commands.effect()
    }

    /// Recover observation of the original operation after losing the
    /// caller's command handle. The returned handle grants observation only.
    pub fn pending_command(&self) -> Option<Command> {
        self.commands.command()
    }

    pub fn shut_queue(&mut self) -> Result<(&'static mut queue::Ring, PhysicalAddress), Error> {
        if self.commands.command().is_some() {
            return Err(Error::Busy);
        }
        // Every live binding depends on retaining or reopening a queue.
        if self.bindings.occupied() {
            self.sequence.next_available()?;
        }
        self.queue
            .as_mut()
            .ok_or(Error::Active)?
            .shut(&mut self.registers, &self.order)?;
        Ok(self
            .queue
            .take()
            .expect("shut queue custody")
            .into_storage())
    }
}

mod binding;
mod command;
mod contract;
pub mod directory;
pub mod queue;
mod registers;
pub mod translation;

#[cfg(test)]
#[path = "../../../../../../../tests/metal/memory/mmu/aarch64/external/smmu/mod.rs"]
mod tests;
