//! Retained requester maintenance and the queue that settles it.

use super::{
    Binding, Error, PhysicalAddress, Sequence,
    queue::{Invalidate, Page, Queue},
    registers::Registers,
};

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

    fn first_step(self) -> Step {
        match self {
            Self::Refreshed(_, Refresh::FirstStage) => Step::FirstStage,
            _ => Step::Directory,
        }
    }

    fn binding(self) -> Binding {
        match self {
            Self::Bound(binding) | Self::Refreshed(binding, _) | Self::Unbound(binding) => binding,
        }
    }

    fn requires_first_stage(self) -> bool {
        matches!(self, Self::Bound(_) | Self::Unbound(_))
    }
}

#[derive(Clone, Copy)]
enum Step {
    Directory,
    FirstStage,
}

struct Pending {
    command: Command,
    effect: Effect,
    step: Step,
}

impl Pending {
    fn operation(&self) -> Invalidate {
        match self.step {
            Step::Directory => Invalidate::Device(self.effect.binding().context.device),
            Step::FirstStage => Invalidate::FirstStage,
        }
    }
}

/// The exclusive queue and the one logical effect it must settle. Only the
/// queue records whether the current step has reached the hardware.
pub(super) struct Commands {
    queue: Queue,
    pending: Option<Pending>,
}

impl Commands {
    pub(super) fn new(queue: Queue) -> Self {
        Self {
            queue,
            pending: None,
        }
    }

    pub(super) fn start(&mut self, registers: &mut Registers<'_>) -> Result<(), Error> {
        let (window, order) = registers.queue_window();
        self.queue.start(window, order)
    }

    pub(super) fn physical(&self) -> PhysicalAddress {
        self.queue.physical()
    }

    pub(super) fn slot(&mut self) -> Result<Slot<'_>, Error> {
        if self.pending.is_some() {
            return Err(Error::Busy);
        }
        Ok(Slot {
            pending: &mut self.pending,
            generation: self.queue.open_generation()?,
        })
    }

    pub(super) fn effect(&self) -> Option<Effect> {
        self.pending.as_ref().map(|pending| pending.effect)
    }

    pub(super) fn command(&self) -> Option<Command> {
        self.pending.as_ref().map(|pending| pending.command)
    }

    pub(super) fn observe(
        &mut self,
        command: Command,
        registers: &mut Registers<'_>,
    ) -> Result<Effect, Error> {
        let pending = self
            .pending
            .as_mut()
            .filter(|pending| pending.command == command)
            .ok_or(Error::Stale)?;
        let (window, order) = registers.queue_window();
        let expected = pending.operation();
        match self.queue.unsettled() {
            None => self.queue.submit(window, order, expected)?,
            Some(operation) if operation == expected => {}
            Some(_) => return Err(Error::Unconfirmed),
        }
        if self.queue.observe(window, order)? != expected {
            return Err(Error::Unconfirmed);
        }
        if matches!(pending.step, Step::Directory) && pending.effect.requires_first_stage() {
            pending.step = Step::FirstStage;
            return Err(Error::Busy);
        }
        let effect = pending.effect;
        self.pending = None;
        Ok(effect)
    }

    pub(super) fn shut(
        &mut self,
        registers: &mut Registers<'_>,
        bound_requesters: bool,
        sequence: &Sequence,
    ) -> Result<(), Error> {
        if self.pending.is_some() {
            return Err(Error::Busy);
        }
        // Every live binding depends on retaining or reopening a queue.
        if bound_requesters {
            sequence.next_available()?;
        }
        let (window, order) = registers.queue_window();
        self.queue.shut(window, order)
    }

    pub(super) fn into_storage(self) -> (&'static mut Page, PhysicalAddress) {
        self.queue.into_storage()
    }
}

/// Exclusive access to the vacant slot of this queue generation.
pub(super) struct Slot<'a> {
    pending: &'a mut Option<Pending>,
    generation: u64,
}

impl Slot<'_> {
    pub(super) fn reserve(
        self,
        controller: PhysicalAddress,
        effect: Effect,
        sequence: &mut Sequence,
    ) -> Result<Command, Error> {
        let command = Command {
            controller,
            queue: self.generation,
            identity: effect.command_identity(sequence)?,
        };
        *self.pending = Some(Pending {
            command,
            effect,
            step: effect.first_step(),
        });
        Ok(command)
    }
}

#[cfg(test)]
#[path = "../../../../../../../tests/metal/memory/mmu/riscv64/external/iommu/command.rs"]
mod tests;
