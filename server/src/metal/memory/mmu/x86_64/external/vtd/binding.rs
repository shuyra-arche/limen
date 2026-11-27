//! Per-requester admission, live binding identity, and retirement.

use super::{
    Controller, Error, GLOBAL_IOTLB, IN_PROGRESS, State,
    command::{Change, CommandId, Operation},
};
use crate::metal::memory::mmu::treaty::{
    BindingChange, BindingCommand, BindingControl, BindingEffect, BindingObservation,
    BindingRejection, BindingSubmission, binding::Sealed,
};
use limen_treaty::model::PhysicalAddress;

/// Resident sequence for this physical controller, preserved through resets
/// and re-admission so a stale binding cannot match a replacement.
pub struct Sequence(pub(super) u64);

impl Sequence {
    pub const FRESH: Self = Self(0);

    pub(super) fn reserve(&mut self, count: u64) -> Result<u64, Error> {
        self.0 = self.0.checked_add(count).ok_or(Error::Exhausted)?;
        Ok(self.0)
    }

    pub(super) fn reserve_binding(
        &mut self,
        unit: PhysicalAddress,
        context: Context,
    ) -> Result<Binding, Error> {
        // An admitted binding can retire even if ordinary work consumes the
        // rest of this controller's sequence.
        let retirement_number = self.reserve(2)?;
        Ok(Binding {
            publication: CommandId {
                unit,
                number: retirement_number - 1,
            },
            retirement_number,
            context,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Context {
    pub requester: u16,
    pub domain: u16,
    pub root: PhysicalAddress,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Refresh {
    GlobalTranslations,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Binding {
    pub(super) publication: CommandId,
    retirement_number: u64,
    pub(super) context: Context,
}

impl Binding {
    pub fn context(self) -> Context {
        self.context
    }

    pub(super) fn retirement(self) -> CommandId {
        CommandId {
            unit: self.publication.unit,
            number: self.retirement_number,
        }
    }
}

impl Controller<'_> {
    fn binding_slot(&self, requester: u16) -> Option<usize> {
        self.storage
            .bindings
            .iter()
            .position(|entry| entry.is_some_and(|binding| binding.context.requester == requester))
    }

    pub fn binding(&self, requester: u16) -> Option<Binding> {
        let slot = self.binding_slot(requester)?;
        match (&self.state, &self.pending) {
            (State::Enabled, None) => self.storage.bindings[slot],
            (
                State::RefreshingContexts | State::RefreshingTranslations,
                Some((_, Operation::Bind(changed) | Operation::Unbind(changed))),
            ) if *changed != slot => self.storage.bindings[slot],
            (State::RefreshingTranslations, Some((_, Operation::Invalidate(changed))))
                if *changed != slot =>
            {
                self.storage.bindings[slot]
            }
            _ => None,
        }
    }

    pub fn pending(&self) -> Option<(CommandId, Change)> {
        let (id, operation) = self.pending.as_ref()?;
        let change = match *operation {
            Operation::Bind(slot) => Change::Bound(self.storage.bindings[slot].unwrap()),
            Operation::Invalidate(slot) => {
                Change::Invalidated(self.storage.bindings[slot].unwrap())
            }
            Operation::Unbind(slot) => Change::Unbound(self.storage.bindings[slot].unwrap()),
        };
        Some((*id, change))
    }

    fn active_slot(&self, expected: Binding) -> Result<usize, Error> {
        if self.state != State::Enabled {
            return Err(Error::Occupied);
        }
        self.binding_slot(expected.context.requester)
            .filter(|&slot| self.storage.bindings[slot] == Some(expected))
            .ok_or(Error::Stale)
    }

    /// Publish a context for a previously blocked requester.
    ///
    /// # Safety
    /// The requester is actually routed to this unit, including topology and
    /// reserved-memory checks. Exclude aliases and ATS. Stop the requester
    /// and drain prior requests through native completion. The root and its
    /// descendants are initialized, pinned and visible to the walker.
    pub unsafe fn begin_bind(&mut self, context: Context) -> Result<CommandId, Error> {
        if self.state != State::Enabled {
            return Err(Error::Occupied);
        }
        let bus = (context.requester >> 8) as u8;
        let device = ((context.requester >> 3) & 31) as u8;
        let function = (context.requester & 7) as u8;
        if !self.supports_39
            || bus != self.storage.bus
            || !(self.unit.includes_all_pci()
                || self.unit.lists_root_endpoint(bus, device, function))
        {
            return Err(Error::Unsupported);
        }
        if context.root == self.storage.root.1 || context.root == self.storage.contexts.1 {
            return Err(Error::Root);
        }
        let entry = self
            .format
            .context_entry(context.root, context.domain)
            .ok_or(Error::Root)?;
        if self.storage.bindings.iter().flatten().any(|binding| {
            binding.context.requester == context.requester
                || binding.context.domain == context.domain
        }) {
            return Err(Error::Occupied);
        }
        let slot = self
            .storage
            .bindings
            .iter()
            .position(Option::is_none)
            .ok_or(Error::Capacity)?;
        let index = usize::from(context.requester as u8);
        if self.storage.contexts.0.0[index] != [0; 2] {
            return Err(Error::Occupied);
        }
        let binding = self
            .storage
            .sequence
            .reserve_binding(self.unit.registers().start(), context)?;
        let command = binding.publication;
        self.storage.bindings[slot] = Some(binding);
        self.pending = Some((command, Operation::Bind(slot)));
        let directory = &mut self.storage.contexts.0.0[index];
        directory[1] = entry[1];
        self.visibility.publish(directory);
        directory[0] = entry[0];
        self.visibility.publish(directory);
        self.invalidate_contexts();
        Ok(command)
    }

    /// Invalidate translations for the exact binding after a visible PTE edit.
    /// This restricted implementation uses a global hardware command.
    ///
    /// # Safety
    /// Serialize page-table edits, retain the old pages and quiesce affected
    /// requesters through native completion. Independent caches and device
    /// transactions require separate settlement.
    pub unsafe fn begin_invalidate(&mut self, expected: Binding) -> Result<CommandId, Error> {
        let slot = self.active_slot(expected)?;
        let command = CommandId {
            unit: self.unit.registers().start(),
            number: self.storage.sequence.reserve(1)?,
        };
        self.pending = Some((command, Operation::Invalidate(slot)));
        self.state = State::RefreshingTranslations;
        self.write(self.iotlb, IN_PROGRESS | GLOBAL_IOTLB | self.drain);
        Ok(command)
    }

    /// Block this requester and start retirement of its original binding.
    ///
    /// # Safety
    /// Exclude old device and fabric requests and keep the requester stopped
    /// through native completion. Retain the old tables and domain until the
    /// matching Unbound receipt. Separate endpoint obligations must settle.
    pub unsafe fn begin_unbind(&mut self, expected: Binding) -> Result<CommandId, Error> {
        let slot = self.active_slot(expected)?;
        let command = expected.retirement();
        let directory =
            &mut self.storage.contexts.0.0[usize::from(expected.context.requester as u8)];
        let original = self
            .format
            .context_entry(expected.context.root, expected.context.domain);
        self.pending = Some((command, Operation::Unbind(slot)));
        if Some(*directory) != original {
            return Err(self.unconfirmed());
        }
        *directory = [0; 2];
        self.visibility.publish(directory);
        self.invalidate_contexts();
        Ok(command)
    }
}

impl Sealed for Controller<'_> {}

impl From<Change> for BindingEffect<Binding, Refresh> {
    fn from(change: Change) -> Self {
        match change {
            Change::Bound(binding) => Self::Bound(binding),
            Change::Invalidated(binding) => Self::Refreshed(binding, Refresh::GlobalTranslations),
            Change::Unbound(binding) => Self::Unbound(binding),
        }
    }
}

impl BindingControl for Controller<'_> {
    type Context = Context;
    type Binding = Binding;
    type Refresh = Refresh;
    type Command = CommandId;
    type Error = Error;

    unsafe fn begin_binding(&mut self, context: Context) -> BindingSubmission<Self> {
        unsafe { self.begin_bind(context) }.map_err(BindingRejection::Rejected)?;
        Ok(self.unsettled_change().expect("accepted binding"))
    }

    unsafe fn begin_refresh(
        &mut self,
        binding: Binding,
        scope: Refresh,
    ) -> BindingSubmission<Self> {
        unsafe { self.begin_invalidate(binding) }
            .map(|command| BindingCommand::new(command, BindingEffect::Refreshed(binding, scope)))
            .map_err(BindingRejection::Rejected)
    }

    unsafe fn begin_retirement(&mut self, binding: Binding) -> BindingSubmission<Self> {
        match unsafe { self.begin_unbind(binding) } {
            Ok(command) => Ok(BindingCommand::new(
                command,
                BindingEffect::Unbound(binding),
            )),
            Err(error @ Error::Unconfirmed) => Err(BindingRejection::Unconfirmed {
                change: self.pending_change(binding).expect("retained retirement"),
                error,
            }),
            Err(error) => Err(BindingRejection::Rejected(error)),
        }
    }

    fn unsettled_change(&self) -> Option<BindingChange<Self>> {
        let (command, effect) = self.pending()?;
        Some(BindingCommand::new(command, effect.into()))
    }

    fn observe_change(&mut self, change: BindingChange<Self>) -> BindingObservation<Self> {
        change.observe(self.unsettled_change(), |command| {
            match self.observe(command) {
                Ok(effect) => Ok(Some(effect.into())),
                Err(Error::Pending) => Ok(None),
                Err(error) => Err(error),
            }
        })
    }
}
