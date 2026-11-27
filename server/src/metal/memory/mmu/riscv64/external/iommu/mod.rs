//! RISC-V IOMMU 1.0 directory and command-queue control.

use crate::metal::memory::access::treaty::device::DeviceWindow;
use limen_treaty::model::PhysicalAddress;

mod binding;
mod command;
mod contract;
pub mod directory;
pub mod queue;
mod registers;
pub mod translation;

pub use binding::{Binding, Sequence};
pub use command::{Command, Effect, Refresh};
pub use directory::{Context, Directory, Storage};
pub use registers::{DeviceOrder, Profile, registers};

use command::Commands;
use directory::Table;
use queue::Queue;
use registers::Registers;

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

/// Exclusive authority over one physical controller and all its resources.
#[must_use]
pub struct Controller<'a> {
    registers: Registers<'a>,
    directory: Table<'a>,
    sequence: &'a mut Sequence,
    commands: Option<Commands>,
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
        let (registers, initial) = unsafe { Registers::admit(window, order) }?;
        let Storage {
            page,
            physical,
            bindings,
            sequence,
        } = storage;
        let profile = registers.profile(initial.mode());
        let directory = Table::admit(page, physical, bindings, profile, initial)?;
        sequence.claim(registers.scope())?;
        Ok(Self {
            registers,
            directory,
            sequence,
            commands: None,
        })
    }

    pub fn profile(&self) -> Profile {
        self.registers.profile(self.directory.last_confirmed_mode())
    }

    /// The retired directory still needs its independent settlement proof.
    pub fn retained_root(&self) -> Option<PhysicalAddress> {
        self.directory.retained_root()
    }

    /// Directory roots whose selection cannot be established.
    pub fn uncertain_roots(&self) -> Option<[Option<PhysicalAddress>; 2]> {
        self.directory.uncertain_roots()
    }

    /// # Safety
    /// Quiesce all affected requesters. Turning translation off does not retire
    /// cached directory entries or complete existing device transactions.
    pub unsafe fn begin_shut(&mut self) -> Result<(), Error> {
        if self.commands.is_some() {
            return Err(Error::Mode);
        }
        self.directory.begin_shut(&mut self.registers)
    }

    /// # Safety
    /// The root must name a fully initialized, pinned and coherent 4 KiB
    /// directory. Retain it and all reachable tables through their hardware
    /// obligations. Keep requesters isolated until old translations and
    /// transactions are independently settled.
    pub unsafe fn begin_install(&mut self) -> Result<(), Error> {
        if self.commands.is_some() {
            return Err(Error::Mode);
        }
        self.directory.begin_install(&mut self.registers)
    }

    /// Observe only the original register write. Delayed readback never
    /// resubmits it, and a contradictory readback blocks further programming.
    pub fn observe_directory(&mut self) -> Result<Directory, Error> {
        self.directory.observe(&mut self.registers)
    }

    pub fn binding(&self, device: u32) -> Option<Binding> {
        if self.directory.selected_mode().ok()? != Directory::One
            || matches!(
                self.unsettled_effect(),
                Some(Effect::Bound(binding) | Effect::Unbound(binding))
                    if binding.context.device == device
            )
        {
            return None;
        }
        self.directory.binding(device)
    }

    /// Build an entry in the as-yet uninstalled directory.
    ///
    /// # Safety
    /// The context root and descendants are pinned device-visible translation
    /// tables for this requester and remain retained until its original binding
    /// is retired. Endpoint routing and requester quiescence are caller-owned.
    pub unsafe fn prepare(&mut self, context: Context) -> Result<Binding, Error> {
        let mode = self.directory.selected_mode()?;
        if mode != Directory::Off || self.directory.retained_root().is_some() {
            return Err(Error::Mode);
        }
        let entry = self
            .directory
            .entry(self.registers.profile(mode), context)?;
        let generation = self.sequence.prepare_binding()?;
        let binding = Binding::new(self.registers.scope().start(), generation, context);
        self.directory
            .publish(entry, binding, self.registers.order());
        Ok(binding)
    }

    fn installed(&self) -> Result<Profile, Error> {
        if self.directory.selected_mode()? != Directory::One {
            return Err(Error::Mode);
        }
        Ok(self.registers.profile(Directory::One))
    }

    /// Reserve and publish a new directory entry, to be confirmed by its
    /// exact directory and translation invalidations.
    ///
    /// # Safety
    /// The new requester must remain stopped until this command settles.
    /// Keep the admitted tables and routing valid as required by prepare.
    pub unsafe fn begin_bind(&mut self, context: Context) -> Result<Command, Error> {
        let profile = self.installed()?;
        let slot = self.commands.as_mut().ok_or(Error::Mode)?.slot()?;
        let entry = self.directory.entry(profile, context)?;
        let generation = self.sequence.next()?;
        let controller = self.registers.scope().start();
        let binding = Binding::new(controller, generation, context);
        let command = slot.reserve(controller, Effect::Bound(binding), self.sequence)?;
        self.directory
            .publish(entry, binding, self.registers.order());
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
        if self.directory.binding(binding.context.device) != Some(binding) {
            return Err(Error::Stale);
        }
        self.installed()?;
        self.commands.as_mut().ok_or(Error::Mode)?.slot()?.reserve(
            self.registers.scope().start(),
            Effect::Refreshed(binding, refresh),
            self.sequence,
        )
    }

    /// Block the original entry and reserve both invalidations before reuse.
    ///
    /// # Safety
    /// Stop and drain the original requester, with no ATS or other endpoint
    /// cache that could continue using the binding. Keep all original storage
    /// pinned until this exact operation settles and independent users retire.
    pub unsafe fn begin_unbind(&mut self, binding: Binding) -> Result<Command, Error> {
        if self.directory.binding(binding.context.device) != Some(binding) {
            return Err(Error::Stale);
        }
        let profile = self.installed()?;
        let command = self.commands.as_mut().ok_or(Error::Mode)?.slot()?.reserve(
            self.registers.scope().start(),
            Effect::Unbound(binding),
            self.sequence,
        )?;
        self.directory
            .block(profile, binding, self.registers.order());
        Ok(command)
    }

    /// Advance the original command and confirm only its own queue evidence.
    /// Incomplete and unconfirmed effects keep the original binding occupied.
    pub fn observe(&mut self, command: Command) -> Result<Effect, Error> {
        let effect = self
            .commands
            .as_mut()
            .ok_or(Error::Stale)?
            .observe(command, &mut self.registers)?;
        if let Effect::Unbound(binding) = effect {
            self.directory
                .retire(self.profile(), binding, self.registers.order());
        }
        Ok(effect)
    }

    /// Borrow only the optional hardware debug-translation registers.
    /// Debug operations are independent from normal requester authority.
    pub fn debug_registers(&mut self) -> Result<DeviceWindow<'_>, Error> {
        self.directory.selected_mode()?;
        if self.commands.is_some() {
            return Err(Error::Mode);
        }
        self.registers.debug_window()
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
        let profile = self.installed()?;
        if self.commands.is_some() || !profile.little_endian_tables {
            return Err(Error::Mode);
        }
        if physical.get() >> profile.address_bits.min(56) != 0 {
            return Err(Error::Address);
        }
        let generation = self.sequence.next_available()?;
        let (window, _) = self.registers.queue_window();
        let queue = unsafe { Queue::prepare(window, page, physical, generation) }?;
        self.sequence.next()?;
        self.commands
            .insert(Commands::new(queue))
            .start(&mut self.registers)
    }

    /// The original queue storage, including an unconfirmed installation.
    pub fn retained_queue(&self) -> Option<PhysicalAddress> {
        self.commands.as_ref().map(Commands::physical)
    }

    /// The exact outstanding operation, including after queue faults.
    pub fn unsettled_effect(&self) -> Option<Effect> {
        self.commands.as_ref().and_then(Commands::effect)
    }

    /// Recover observation of the original operation after losing the
    /// caller's command handle. The returned handle grants observation only.
    pub fn pending_command(&self) -> Option<Command> {
        self.commands.as_ref().and_then(Commands::command)
    }

    pub fn shut_queue(&mut self) -> Result<(&'static mut queue::Page, PhysicalAddress), Error> {
        self.commands.as_mut().ok_or(Error::Mode)?.shut(
            &mut self.registers,
            self.directory.occupied(),
            self.sequence,
        )?;
        Ok(self
            .commands
            .take()
            .expect("shut queue custody")
            .into_storage())
    }
}

#[cfg(test)]
#[path = "../../../../../../../tests/metal/memory/mmu/riscv64/external/iommu/mod.rs"]
mod tests;
