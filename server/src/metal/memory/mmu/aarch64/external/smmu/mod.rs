//! Arm SMMUv3 non-secure stage-two control for one physical interface.

use crate::metal::{
    memory::mmu::access::device::DeviceWindow,
    platform::description::devicetree::{Error as TreeError, Node},
};
pub use directory::{Binding, Context, Sequence, Storage};
use limen_treaty::model::{PhysicalAddress, PhysicalRange};
use queue::{Invalidate, Queue};

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
    Shutting(StreamTable),
    Stopped(StreamTable),
    Unconfirmed(Option<StreamTable>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Command {
    controller: PhysicalAddress,
    queue: u64,
    identity: CommandIdentity,
}

/// A binding pre-reserves exactly one retirement identity when admitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CommandIdentity {
    Ordinary(u64),
    Retirement(u64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refresh {
    Stream,
    StageTwo,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    Bound(Binding),
    Refreshed(Binding, Refresh),
    Unbound(Binding),
}

impl Effect {
    fn command_identity(self, sequence: &mut Sequence) -> Result<CommandIdentity, Error> {
        Ok(match self {
            Self::Bound(binding) => CommandIdentity::Ordinary(binding.generation),
            Self::Refreshed(_, _) => CommandIdentity::Ordinary(sequence.next()?),
            Self::Unbound(binding) => CommandIdentity::Retirement(binding.generation),
        })
    }

    fn operation(self, second: bool) -> Option<Invalidate> {
        match (self, second) {
            (Self::Bound(binding) | Self::Unbound(binding), false)
            | (Self::Refreshed(binding, Refresh::Stream), false) => {
                Some(Invalidate::Stream(binding.context.stream))
            }
            (Self::Bound(binding) | Self::Unbound(binding), true)
            | (Self::Refreshed(binding, Refresh::StageTwo), false) => {
                Some(Invalidate::StageTwo(binding.context.vmid))
            }
            _ => None,
        }
    }
}

struct Pending {
    command: Command,
    effect: Effect,
    issued: bool,
    second: bool,
}

/// The disabled policy, stream directory and command queue retain the same
/// exclusive register authority through every transition.
#[must_use]
pub struct Controller<'a> {
    window: DeviceWindow<'a>,
    storage: Storage<'a>,
    bits: u8,
    pending: Option<Pending>,
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
        let idr1 = unsafe { read(window, IDR1)? };
        if idr1 & (1 << 30) != 0 {
            return Err(Error::Rejected);
        }
        let bits = storage.geometry(idr1 & 63)?;
        storage.validate()?;
        let profile = Profile {
            stage_two: idr0 & 1 != 0,
            granule_4k: idr5 & (1 << 4) != 0,
            enabled: false,
            aarch64_tables: (idr0 >> 2) & 2 != 0,
        };
        translation::Format::new(profile)?;
        storage.sequence.claim(window.physical())?;
        Ok(Self {
            window: window.reborrow(),
            storage,
            bits,
            pending: None,
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
            State::Installing(table)
            | State::Enabled(table)
            | State::Shutting(table)
            | State::Stopped(table) => Some(table),
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
        if unsafe { read(&mut self.window, GBPA)? } & UPDATE != 0 {
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
        let policy = unsafe { read(&mut self.window, GBPA)? };
        if policy & UPDATE != 0 {
            return Err(Error::Busy);
        }
        if policy & ABORT == 0
            || unsafe { read(&mut self.window, CR0)? | read(&mut self.window, CR0_ACK)? } != 0
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
    pub unsafe fn begin_install(&mut self) -> Result<(), Error> {
        match self.state {
            State::Blocked if self.queue.is_none() => {}
            State::Unconfirmed(_) => return Err(Error::Unconfirmed),
            _ => return Err(Error::Active),
        }
        let base = self.storage.physical;
        let bits = self.bits;
        let address = base.get();
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
        if unsafe { read(&mut self.window, STREAM_CONFIG)? } & 0x3ffff != u32::from(bits)
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
        if unsafe { read(&mut self.window, CR0)? } != SMMU_ENABLE {
            self.state = State::Unconfirmed(Some(table));
            return Err(Error::Unconfirmed);
        }
        if unsafe { read(&mut self.window, CR0_ACK)? } != SMMU_ENABLE {
            return Err(Error::Busy);
        }
        self.profile.enabled = true;
        self.state = State::Enabled(table);
        Ok(())
    }

    fn binding_for(&self, context: Context, generation: u64) -> Binding {
        Binding {
            controller: self.window.physical().start(),
            generation,
            context,
        }
    }

    pub fn binding(&self, stream: u32) -> Option<Binding> {
        if !matches!(self.state, State::Enabled(_))
            || self
                .pending
                .as_ref()
                .is_some_and(|pending| match pending.effect {
                    Effect::Bound(binding) | Effect::Unbound(binding) => {
                        binding.context.stream == stream
                    }
                    Effect::Refreshed(_, _) => false,
                })
        {
            return None;
        }
        self.storage.binding(stream)
    }

    /// Fill one STE before installing the table.
    ///
    /// # Safety
    /// Retain the authorized stage-two translation and checked routing for the
    /// original stream. Its subordinate pages must remain pinned and visible
    /// until the binding and independent endpoint obligations have completed.
    pub unsafe fn prepare(&mut self, context: Context) -> Result<Binding, Error> {
        if !matches!(self.state, State::Blocked) {
            return Err(Error::Active);
        }
        let (slot, words) = self.storage.entry(self.format()?, context)?;
        let generation = self.storage.sequence.prepare_binding()?;
        let binding = self.binding_for(context, generation);
        self.storage.publish(slot, binding, words, &self.order);
        Ok(binding)
    }

    fn available_queue(&self) -> Result<u64, Error> {
        if !matches!(self.state, State::Enabled(_)) {
            return Err(Error::Active);
        }
        if self.pending.is_some() {
            return Err(Error::Busy);
        }
        self.queue.as_ref().ok_or(Error::Active)?.open_generation()
    }

    fn reserve(&mut self, queue: u64, effect: Effect) -> Result<Command, Error> {
        let command = Command {
            controller: self.window.physical().start(),
            queue,
            identity: effect.command_identity(self.storage.sequence)?,
        };
        self.pending = Some(Pending {
            command,
            effect,
            issued: false,
            second: false,
        });
        Ok(command)
    }

    /// Publish a new STE and reserve its exact configuration and TLB commands.
    ///
    /// # Safety
    /// The stream remains quiescent until this command settles; retain the
    /// translation and exact routing as required by prepare.
    pub unsafe fn begin_bind(&mut self, context: Context) -> Result<Command, Error> {
        let queue = self.available_queue()?;
        let (slot, words) = self.storage.entry(self.format()?, context)?;
        let generation = self.storage.sequence.next()?;
        let binding = self.binding_for(context, generation);
        let command = self.reserve(queue, Effect::Bound(binding))?;
        self.storage.publish(slot, binding, words, &self.order);
        Ok(command)
    }

    /// Associate one new maintenance command with the exact current binding.
    ///
    /// # Safety
    /// Subordinate page-table changes are visible and both old and new pages
    /// remain pinned until the original command and endpoint effects complete.
    pub unsafe fn begin_refresh(
        &mut self,
        binding: Binding,
        refresh: Refresh,
    ) -> Result<Command, Error> {
        if self.storage.binding(binding.context.stream) != Some(binding) {
            return Err(Error::Stale);
        }
        let queue = self.available_queue()?;
        self.reserve(queue, Effect::Refreshed(binding, refresh))
    }

    /// Block and invalidate an exact original binding.
    ///
    /// # Safety
    /// Stop and drain the original stream, with no ATS or endpoint cache
    /// that can still refer to its binding. Retain all original storage
    /// until its original cache invalidations and independent users settle.
    pub unsafe fn begin_unbind(&mut self, binding: Binding) -> Result<Command, Error> {
        if self.storage.binding(binding.context.stream) != Some(binding) {
            return Err(Error::Stale);
        }
        let queue = self.available_queue()?;
        let command = self.reserve(queue, Effect::Unbound(binding))?;
        self.storage.block(binding, &self.order);
        Ok(command)
    }

    pub fn observe(&mut self, command: Command) -> Result<Effect, Error> {
        let pending = self
            .pending
            .as_mut()
            .filter(|pending| pending.command == command)
            .ok_or(Error::Stale)?;
        let queue = self
            .queue
            .as_mut()
            .expect("an admitted command keeps its queue");
        if queue.generation() != command.queue {
            return Err(Error::Unconfirmed);
        }
        let operation = pending
            .effect
            .operation(pending.second)
            .expect("pending phase");
        if !pending.issued {
            let submitted = queue.submit(&mut self.window, &self.order, operation);
            pending.issued = submitted.is_ok() || queue.unsettled().is_some();
            submitted?;
        }
        let actual = queue.observe(&mut self.window, &self.order)?;
        if actual != operation {
            return Err(Error::Unconfirmed);
        }
        if !pending.second && pending.effect.operation(true).is_some() {
            pending.second = true;
            pending.issued = false;
            return Err(Error::Busy);
        }
        let effect = pending.effect;
        if let Effect::Unbound(binding) = effect {
            self.storage.retire(binding, &self.order);
        }
        self.pending = None;
        Ok(effect)
    }

    /// Disable the interface after all bindings and the queue have retired.
    ///
    /// # Safety
    /// Keep requesters isolated and retain any independently outstanding
    /// hardware obligation. The original table remains in custody even
    /// after the controller reports a disabled interface.
    pub unsafe fn begin_shut(&mut self) -> Result<(), Error> {
        let State::Enabled(table) = self.state else {
            return Err(Error::Active);
        };
        if self.queue.is_some() || self.pending.is_some() || self.storage.occupied() {
            return Err(Error::Active);
        }
        self.state = State::Shutting(table);
        self.order.fence();
        unsafe { self.window.write_u32(CR0, 0) }.map_err(|_| Error::Window)
    }

    pub fn observe_shut(&mut self) -> Result<(), Error> {
        let table = match self.state {
            State::Shutting(table) => table,
            State::Unconfirmed(_) => return Err(Error::Unconfirmed),
            _ => return Err(Error::Active),
        };
        self.order.fence();
        if unsafe { read(&mut self.window, CR0)? } != 0
            || unsafe { read(&mut self.window, GBPA)? } & (UPDATE | ABORT) != ABORT
        {
            self.state = State::Unconfirmed(Some(table));
            return Err(Error::Unconfirmed);
        }
        if unsafe { read(&mut self.window, CR0_ACK)? } != 0 {
            return Err(Error::Busy);
        }
        self.profile.enabled = false;
        self.state = State::Stopped(table);
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
            State::Enabled(_) if self.queue.is_none() => {}
            State::Unconfirmed(_) => return Err(Error::Unconfirmed),
            _ => return Err(Error::Active),
        }
        let generation = self.storage.sequence.next_available()?;
        let queue = unsafe { Queue::prepare(&mut self.window, ring, physical, generation) }?;
        self.storage.sequence.next()?;
        self.queue
            .insert(queue)
            .start(&mut self.window, &self.order)
    }

    /// The original queue storage, including an unconfirmed installation.
    pub fn retained_queue(&self) -> Option<PhysicalAddress> {
        self.queue.as_ref().map(Queue::physical)
    }

    /// The exact outstanding binding operation, retained across queue faults.
    pub fn unsettled_effect(&self) -> Option<Effect> {
        self.pending.as_ref().map(|pending| pending.effect)
    }

    /// Recover observation of the original operation after losing the
    /// caller's command handle. The returned handle grants observation only.
    pub fn pending_command(&self) -> Option<Command> {
        self.pending.as_ref().map(|pending| pending.command)
    }

    /// Recover the generic retirement ticket for exactly this original binding.
    pub fn pending_retirement(&self, binding: Binding) -> Option<Unbinding> {
        self.pending.as_ref().and_then(|pending| {
            (pending.effect == Effect::Unbound(binding)).then_some(Unbinding {
                command: pending.command,
                binding,
            })
        })
    }

    pub fn shut_queue(&mut self) -> Result<(&'static mut queue::Ring, PhysicalAddress), Error> {
        if self.pending.is_some() {
            return Err(Error::Busy);
        }
        // Every live binding depends on retaining or reopening a queue.
        if self.storage.occupied() {
            self.storage.sequence.next_available()?;
        }
        self.queue
            .as_mut()
            .ok_or(Error::Active)?
            .shut(&mut self.window, &self.order)?;
        Ok(self
            .queue
            .take()
            .expect("shut queue custody")
            .into_storage())
    }
}

unsafe fn read(window: &mut DeviceWindow<'_>, offset: usize) -> Result<u32, Error> {
    unsafe { window.read_u32(offset) }
        .map(u32::from_le)
        .map_err(|_| Error::Window)
}

pub mod directory;
pub mod queue;
pub mod translation;

#[cfg(test)]
#[path = "../../../../../../../tests/metal/memory/mmu/aarch64/external/smmu/mod.rs"]
mod tests;

/// Original controller command and the binding it is retiring.
pub struct Unbinding {
    command: Command,
    binding: Binding,
}

impl crate::metal::memory::mmu::retirement::Sealed for Controller<'_> {}

impl crate::metal::memory::mmu::Unbind for Controller<'_> {
    type Binding = Binding;
    type Ticket = Unbinding;
    type Error = Error;

    unsafe fn retire(&mut self, binding: Binding) -> Result<Unbinding, Error> {
        let command = unsafe { self.begin_unbind(binding) }?;
        Ok(Unbinding { command, binding })
    }

    fn observe_retirement(
        &mut self,
        ticket: &Unbinding,
    ) -> Result<crate::metal::memory::mmu::Retirement, Error> {
        use crate::metal::memory::mmu::Retirement;
        match self.observe(ticket.command) {
            Ok(Effect::Unbound(binding)) if binding == ticket.binding => Ok(Retirement::Settled),
            Ok(_) => Err(Error::Unconfirmed),
            Err(Error::Busy) => Ok(Retirement::Pending),
            Err(error) => Err(error),
        }
    }
}
