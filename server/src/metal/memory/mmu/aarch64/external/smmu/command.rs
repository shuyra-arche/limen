//! Original binding maintenance effects and the ordered queue work they require.

use super::{
    Binding, DeviceOrder, Error, Registers,
    queue::{Invalidate, Queue},
};
use limen_treaty::model::{PhysicalAddress, PhysicalRange};

/// The original physical interface's resident sequence. Keep it through
/// controller resets and admissions to prevent old handles from matching.
pub struct Sequence {
    scope: Option<PhysicalRange>,
    number: u64,
}

impl Sequence {
    pub const FRESH: Self = Self {
        scope: None,
        number: 0,
    };

    pub(super) fn claim(&mut self, scope: PhysicalRange) -> Result<(), Error> {
        if self.scope.is_some_and(|original| original != scope) {
            return Err(Error::Rejected);
        }
        self.scope = Some(scope);
        Ok(())
    }

    pub(super) fn next_available(&self) -> Result<u64, Error> {
        self.number.checked_add(1).ok_or(Error::Exhausted)
    }

    pub(super) fn next(&mut self) -> Result<u64, Error> {
        self.number = self.next_available()?;
        Ok(self.number)
    }

    /// A prepared binding must leave an identity for its first command queue.
    pub(super) fn prepare_binding(&mut self) -> Result<u64, Error> {
        let generation = self.next_available()?;
        if generation == u64::MAX {
            return Err(Error::Exhausted);
        }
        self.next()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Command {
    controller: PhysicalAddress,
    queue: u64,
    identity: CommandIdentity,
}

/// The binding generation pre-reserves a separate retirement identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CommandIdentity {
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
    pub(super) fn command_identity(
        self,
        sequence: &mut Sequence,
    ) -> Result<CommandIdentity, Error> {
        Ok(match self {
            Self::Bound(binding) => CommandIdentity::Ordinary(binding.generation),
            Self::Refreshed(_, _) => CommandIdentity::Ordinary(sequence.next()?),
            Self::Unbound(binding) => CommandIdentity::Retirement(binding.generation),
        })
    }

    fn binding(self) -> Binding {
        match self {
            Self::Bound(binding) | Self::Refreshed(binding, _) | Self::Unbound(binding) => binding,
        }
    }

    fn first(self) -> Step {
        match self {
            Self::Refreshed(_, Refresh::StageTwo) => Step::StageTwo,
            _ => Step::Stream,
        }
    }

    fn next(self, completed: Step) -> Option<Step> {
        matches!(
            (self, completed),
            (Self::Bound(_) | Self::Unbound(_), Step::Stream)
        )
        .then_some(Step::StageTwo)
    }

    pub(super) fn hides_binding(self, stream: u32) -> bool {
        match self {
            Self::Bound(binding) | Self::Unbound(binding) => binding.context.stream == stream,
            Self::Refreshed(_, _) => false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Stream,
    StageTwo,
    Unconfirmed,
}

impl Step {
    fn operation(self, binding: Binding) -> Result<Invalidate, Error> {
        match self {
            Self::Stream => Ok(Invalidate::Stream(binding.context.stream)),
            Self::StageTwo => Ok(Invalidate::StageTwo(binding.context.vmid)),
            Self::Unconfirmed => Err(Error::Unconfirmed),
        }
    }
}

struct Pending {
    command: Command,
    effect: Effect,
    step: Step,
}

/// One retained binding effect. The physical queue owns its own positions and
/// command-batch completion, independent of this binding operation's phases.
pub(super) struct Commands {
    pending: Option<Pending>,
}

impl Commands {
    pub(super) const fn new() -> Self {
        Self { pending: None }
    }

    pub(super) fn command(&self) -> Option<Command> {
        self.pending.as_ref().map(|pending| pending.command)
    }

    pub(super) fn effect(&self) -> Option<Effect> {
        self.pending.as_ref().map(|pending| pending.effect)
    }

    pub(super) fn reserve(
        &mut self,
        controller: PhysicalAddress,
        queue: u64,
        effect: Effect,
        sequence: &mut Sequence,
    ) -> Result<Command, Error> {
        if self.pending.is_some() {
            return Err(Error::Busy);
        }
        let command = Command {
            controller,
            queue,
            identity: effect.command_identity(sequence)?,
        };
        self.pending = Some(Pending {
            command,
            effect,
            step: effect.first(),
        });
        Ok(command)
    }

    pub(super) fn observe(
        &mut self,
        command: Command,
        queue: Option<&mut Queue>,
        registers: &mut Registers<'_>,
        order: &DeviceOrder,
    ) -> Result<Effect, Error> {
        let pending = self
            .pending
            .as_mut()
            .filter(|pending| pending.command == command)
            .ok_or(Error::Stale)?;
        let queue = queue.expect("an admitted command keeps its queue");
        if queue.generation() != command.queue {
            return Err(Error::Unconfirmed);
        }
        let operation = pending.step.operation(pending.effect.binding())?;
        // The queue keeps the original batch even if its producer write is
        // uncertain. Only a step without a batch can issue one.
        match queue.unsettled() {
            None => queue.submit(registers, order, operation)?,
            Some(actual) if actual == operation => {}
            Some(_) => {
                pending.step = Step::Unconfirmed;
                return Err(Error::Unconfirmed);
            }
        }
        if queue.observe(registers, order)? != operation {
            pending.step = Step::Unconfirmed;
            return Err(Error::Unconfirmed);
        }
        if let Some(next) = pending.effect.next(pending.step) {
            pending.step = next;
            return Err(Error::Busy);
        }
        let effect = pending.effect;
        self.pending = None;
        Ok(effect)
    }
}

#[cfg(test)]
#[path = "../../../../../../../tests/metal/memory/mmu/aarch64/external/smmu/command.rs"]
mod tests;
