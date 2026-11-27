//! RISC-V IOMMU 1.0 directory and command-queue control.

use crate::metal::{
    memory::mmu::access::device::DeviceWindow,
    platform::description::devicetree::{Error as TreeError, Node},
};
use limen_treaty::model::{ByteLength, PhysicalAddress, PhysicalRange};

pub use directory::{Binding, Context, Sequence, Storage};
use queue::{Invalidate, Queue};

const CAPABILITIES: usize = 0;
const CONTROL: usize = 8;
const DDTP: usize = 0x10;
const MODE: u64 = 0xf;
const BUSY: u64 = 1 << 4;
const PPN: u64 = ((1 << 44) - 1) << 10;

pub fn registers(node: Node<'_, '_>) -> Result<Option<PhysicalRange>, TreeError> {
    if !node.enabled()? || !node.compatible(b"riscv,iommu")? {
        return Ok(None);
    }
    if node.property(b"#iommu-cells")? != Some(&1u32.to_be_bytes()) {
        return Err(TreeError::Unsupported);
    }
    Ok(Some(node.register(0)?))
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Window,
    Version,
    Mode,
    Busy,
    Address,
    Rejected,
    Unconfirmed,
    Capacity,
    Exhausted,
    Stale,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Directory {
    Off,
    Bare,
    One,
    Two,
    Three,
}

impl Directory {
    fn decode(word: u64) -> Result<Self, Error> {
        match word & MODE {
            0 => Ok(Self::Off),
            1 => Ok(Self::Bare),
            2 => Ok(Self::One),
            3 => Ok(Self::Two),
            4 => Ok(Self::Three),
            _ => Err(Error::Mode),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Profile {
    pub sv39: bool,
    pub debug_translation: bool,
    extended_context: bool,
    pub little_endian_tables: bool,
    pub address_bits: u32,
    pub directory: Directory,
}

/// Device and coherent-memory ordering supplied by the native Hart.
pub struct DeviceOrder(fn());

impl DeviceOrder {
    #[cfg(all(limen_target, target_arch = "riscv64"))]
    pub fn native() -> Self {
        Self(|| unsafe { core::arch::asm!("fence iorw, iorw", options(nostack)) })
    }

    fn fence(&self) {
        (self.0)();
    }
}

#[derive(Clone, Copy)]
struct DirectoryWord(u64);

impl DirectoryWord {
    fn mode(self) -> Directory {
        Directory::decode(self.0).expect("checked directory mode")
    }

    fn root(self) -> Option<PhysicalAddress> {
        match self.mode() {
            Directory::Off | Directory::Bare => None,
            Directory::One | Directory::Two | Directory::Three => {
                Some(PhysicalAddress::new((self.0 & PPN) << 2))
            }
        }
    }

    fn off(self) -> Self {
        // The PPN must remain unchanged on entry to Off. Preserve WPRI fields.
        Self(self.0 & !MODE)
    }

    fn one(self, root: PhysicalAddress, address_bits: u32) -> Result<Self, Error> {
        let address = root.get();
        if !address.is_multiple_of(4096) || address >> address_bits.min(56) != 0 {
            return Err(Error::Address);
        }
        Ok(Self((self.0 & !(PPN | MODE)) | (address >> 2) | 2))
    }

    fn confirms(self, target: Self) -> bool {
        self.mode() == target.mode()
            && (target.mode() == Directory::Off || self.0 & PPN == target.0 & PPN)
    }
}

enum Selection {
    Selected(DirectoryWord),
    Pending {
        original: DirectoryWord,
        target: DirectoryWord,
    },
    Unconfirmed {
        original: DirectoryWord,
        target: DirectoryWord,
    },
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
    Directory,
    FirstStage,
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
            | (Self::Refreshed(binding, Refresh::Directory), false) => {
                Some(Invalidate::Device(binding.context.device))
            }
            (Self::Bound(_) | Self::Unbound(_), true)
            | (Self::Refreshed(_, Refresh::FirstStage), false) => Some(Invalidate::FirstStage),
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

/// Exclusive authority over a physical register file. Retains the installed
/// directory, all unsettled directory writes and the optional command queue.
#[must_use]
pub struct Controller<'a> {
    window: DeviceWindow<'a>,
    storage: Storage<'a>,
    pending: Option<Pending>,
    order: DeviceOrder,
    profile: Profile,
    directory: Selection,
    retained_root: Option<PhysicalAddress>,
    queue: Option<Queue>,
}

impl<'a> Controller<'a> {
    /// # Safety
    /// Map the actual little-endian IOMMU register file, exclusively control
    /// the device and its requesters, and retain translation and queue resources
    /// through all their hardware obligations, including loss of this owner.
    /// The provided physical address must name the complete pinned directory
    /// page and all device-visible RAM must be coherent. Retain the original
    /// sequence across this controller's resets and re-admissions. Only use
    /// FRESH for a physical scope with no previously issued identities.
    pub unsafe fn admit(
        window: &'a mut DeviceWindow<'_>,
        order: DeviceOrder,
        storage: Storage<'a>,
    ) -> Result<Self, Error> {
        let caps =
            u64::from_le(unsafe { window.read_u64(CAPABILITIES) }.map_err(|_| Error::Window)?);
        if caps as u8 != 0x10 {
            return Err(Error::Version);
        }
        let control = u32::from_le(unsafe { window.read_u32(CONTROL) }.map_err(|_| Error::Window)?);
        let directory = unsafe { read_directory(window)? };
        if !matches!(directory.mode(), Directory::Off | Directory::Bare) {
            // An inherited live directory cannot be transferred without its
            // original storage and outstanding responsibilities.
            return Err(Error::Mode);
        }
        let profile = Profile {
            extended_context: caps & (1 << 22) != 0,
            debug_translation: caps & (1 << 31) != 0,
            little_endian_tables: control & 1 == 0,
            address_bits: ((caps >> 32) & 0x3f) as u32,
            sv39: caps & (1 << 9) != 0,
            directory: directory.mode(),
        };
        storage.validate(profile)?;
        directory.one(storage.physical, profile.address_bits)?;
        storage.sequence.claim(window.physical())?;
        Ok(Self {
            window: window.reborrow(),
            storage,
            pending: None,
            order,
            profile,
            directory: Selection::Selected(directory),
            retained_root: None,
            queue: None,
        })
    }

    pub fn profile(&self) -> Profile {
        self.profile
    }

    /// The retired directory still needs its independent settlement proof.
    pub fn retained_root(&self) -> Option<PhysicalAddress> {
        self.retained_root
    }

    /// Directory roots whose selection cannot be established.
    pub fn uncertain_roots(&self) -> Option<[Option<PhysicalAddress>; 2]> {
        match self.directory {
            Selection::Unconfirmed { original, target } => Some([original.root(), target.root()]),
            _ => None,
        }
    }

    fn selected(&self) -> Result<DirectoryWord, Error> {
        match self.directory {
            Selection::Selected(word) => Ok(word),
            Selection::Pending { .. } => Err(Error::Busy),
            Selection::Unconfirmed { .. } => Err(Error::Unconfirmed),
        }
    }

    fn issue_directory(
        &mut self,
        original: DirectoryWord,
        target: DirectoryWord,
    ) -> Result<(), Error> {
        if self.queue.is_some() || self.pending.is_some() {
            return Err(Error::Mode);
        }
        self.directory = Selection::Pending { original, target };
        self.order.fence();
        unsafe { self.window.write_u64(DDTP, target.0.to_le()) }.map_err(|_| Error::Window)
    }

    /// # Safety
    /// Quiesce all affected requesters. Turning translation off does not retire
    /// cached directory entries or complete existing device transactions.
    pub unsafe fn begin_shut(&mut self) -> Result<(), Error> {
        let original = self.selected()?;
        if self.storage.occupied() {
            return Err(Error::Mode);
        }
        self.issue_directory(original, original.off())
    }

    /// # Safety
    /// The root must name a fully initialized, pinned and coherent 4 KiB
    /// directory. Retain it and all reachable tables through their hardware
    /// obligations. Keep requesters isolated until old translations and
    /// transactions are independently settled.
    pub unsafe fn begin_install(&mut self) -> Result<(), Error> {
        let original = self.selected()?;
        if original.mode() != Directory::Off || self.retained_root.is_some() {
            return Err(Error::Mode);
        }
        let target = original.one(self.storage.physical, self.profile.address_bits)?;
        self.issue_directory(original, target)
    }

    /// Observe only the original register write. Delayed readback never
    /// resubmits it, and a contradictory readback blocks further programming.
    pub fn observe_directory(&mut self) -> Result<Directory, Error> {
        let Selection::Pending { original, target } = self.directory else {
            self.selected()?;
            return Err(Error::Mode);
        };
        self.order.fence();
        let actual = match unsafe { read_directory(&mut self.window) } {
            Err(Error::Mode) => {
                self.directory = Selection::Unconfirmed { original, target };
                return Err(Error::Unconfirmed);
            }
            result => result?,
        };
        if !actual.confirms(target) {
            self.directory = Selection::Unconfirmed { original, target };
            return Err(Error::Unconfirmed);
        }
        if actual.mode() == Directory::Off
            && let Some(root) = original.root()
        {
            self.retained_root = Some(root);
        }
        self.profile.directory = actual.mode();
        self.directory = Selection::Selected(actual);
        Ok(actual.mode())
    }

    fn binding_for(&self, context: Context, generation: u64) -> Binding {
        Binding {
            controller: self.window.physical().start(),
            generation,
            context,
        }
    }

    pub fn binding(&self, device: u32) -> Option<Binding> {
        if self.selected().ok()?.mode() != Directory::One
            || self
                .pending
                .as_ref()
                .is_some_and(|pending| match pending.effect {
                    Effect::Bound(binding) | Effect::Unbound(binding) => {
                        binding.context.device == device
                    }
                    Effect::Refreshed(_, _) => false,
                })
        {
            return None;
        }
        self.storage.binding(device)
    }

    /// Build an entry in the as-yet uninstalled directory.
    ///
    /// # Safety
    /// The context root and descendants are pinned device-visible translation
    /// tables for this requester and remain retained until its original binding
    /// is retired. Endpoint routing and requester quiescence are caller-owned.
    pub unsafe fn prepare(&mut self, context: Context) -> Result<Binding, Error> {
        if self.selected()?.mode() != Directory::Off || self.retained_root.is_some() {
            return Err(Error::Mode);
        }
        let (slot, words) = self.storage.entry(self.profile, context)?;
        let generation = self.storage.sequence.prepare_binding()?;
        let binding = self.binding_for(context, generation);
        self.storage
            .publish(self.profile, slot, binding, words, &self.order);
        Ok(binding)
    }

    fn available_queue(&self) -> Result<u64, Error> {
        if self.selected()?.mode() != Directory::One {
            return Err(Error::Mode);
        }
        if self.pending.is_some() {
            return Err(Error::Busy);
        }
        self.queue.as_ref().ok_or(Error::Mode)?.open_generation()
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

    /// Reserve and publish a new directory entry, to be confirmed by its
    /// exact directory and translation invalidations.
    ///
    /// # Safety
    /// The new requester must remain stopped until this command settles.
    /// Keep the admitted tables and routing valid as required by prepare.
    pub unsafe fn begin_bind(&mut self, context: Context) -> Result<Command, Error> {
        let queue = self.available_queue()?;
        let (slot, words) = self.storage.entry(self.profile, context)?;
        let generation = self.storage.sequence.next()?;
        let binding = self.binding_for(context, generation);
        let command = self.reserve(queue, Effect::Bound(binding))?;
        self.storage
            .publish(self.profile, slot, binding, words, &self.order);
        Ok(command)
    }

    /// Associate a new maintenance command with the exact current binding.
    ///
    /// # Safety
    /// Any changed table words must be visible and both old and new table
    /// resources remain pinned through settlement. The requester has stopped
    /// and any separate endpoint cache or transaction obligation is covered.
    pub unsafe fn begin_refresh(
        &mut self,
        binding: Binding,
        refresh: Refresh,
    ) -> Result<Command, Error> {
        if self.storage.binding(binding.context.device) != Some(binding) {
            return Err(Error::Stale);
        }
        let queue = self.available_queue()?;
        self.reserve(queue, Effect::Refreshed(binding, refresh))
    }

    /// Block the original entry and reserve both invalidations before reuse.
    ///
    /// # Safety
    /// Stop and drain the original requester, with no ATS or other endpoint
    /// cache that could continue using the binding. Keep all original storage
    /// pinned until this exact operation settles and independent users retire.
    pub unsafe fn begin_unbind(&mut self, binding: Binding) -> Result<Command, Error> {
        if self.storage.binding(binding.context.device) != Some(binding) {
            return Err(Error::Stale);
        }
        let queue = self.available_queue()?;
        let command = self.reserve(queue, Effect::Unbound(binding))?;
        self.storage.block(self.profile, binding, &self.order);
        Ok(command)
    }

    /// Advance the original command and confirm only its own queue evidence.
    /// Incomplete and unconfirmed effects keep the original binding occupied.
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
            self.storage.retire(self.profile, binding, &self.order);
        }
        self.pending = None;
        Ok(effect)
    }

    /// Borrow only the optional hardware debug-translation registers.
    /// Debug operations are independent from normal requester authority.
    pub fn debug_registers(&mut self) -> Result<DeviceWindow<'_>, Error> {
        self.selected()?;
        if !self.profile.debug_translation || self.queue.is_some() {
            return Err(Error::Mode);
        }
        let physical = self
            .window
            .physical()
            .start()
            .advance(ByteLength::new(0x258))
            .and_then(|start| start.range(ByteLength::new(0x18)))
            .ok_or(Error::Window)?;
        self.window.subrange(physical).map_err(|_| Error::Window)
    }

    /// # Safety
    /// The page must be exclusively owned, at exactly `physical` in pinned,
    /// coherent memory, and retained through shutdown, faults, or loss of this
    /// controller. No independent producer may access the queue.
    pub unsafe fn open_queue(
        &mut self,
        page: &'static mut queue::Page,
        physical: PhysicalAddress,
    ) -> Result<(), Error> {
        if self.selected()?.mode() != Directory::One
            || self.queue.is_some()
            || !self.profile.little_endian_tables
        {
            return Err(Error::Mode);
        }
        if physical.get() >> self.profile.address_bits.min(56) != 0 {
            return Err(Error::Address);
        }
        let generation = self.storage.sequence.next_available()?;
        let queue = unsafe { Queue::prepare(&mut self.window, page, physical, generation) }?;
        self.storage.sequence.next()?;
        self.queue
            .insert(queue)
            .start(&mut self.window, &self.order)
    }

    /// The original queue storage, including an unconfirmed installation.
    pub fn retained_queue(&self) -> Option<PhysicalAddress> {
        self.queue.as_ref().map(Queue::physical)
    }

    /// The exact outstanding operation, including after queue faults.
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

    pub fn shut_queue(&mut self) -> Result<(&'static mut queue::Page, PhysicalAddress), Error> {
        if self.pending.is_some() {
            return Err(Error::Busy);
        }
        // Every live binding depends on retaining or reopening a queue.
        if self.storage.occupied() {
            self.storage.sequence.next_available()?;
        }
        self.queue
            .as_mut()
            .ok_or(Error::Mode)?
            .shut(&mut self.window, &self.order)?;
        Ok(self
            .queue
            .take()
            .expect("shut queue custody")
            .into_storage())
    }
}

unsafe fn read_directory(window: &mut DeviceWindow<'_>) -> Result<DirectoryWord, Error> {
    let word = u64::from_le(unsafe { window.read_u64(DDTP) }.map_err(|_| Error::Window)?);
    if word & BUSY != 0 {
        return Err(Error::Busy);
    }
    Directory::decode(word)?;
    Ok(DirectoryWord(word))
}

pub mod directory;
pub mod queue;
pub mod translation;

#[cfg(test)]
#[path = "../../../../../../../tests/metal/memory/mmu/riscv64/external/iommu/mod.rs"]
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
