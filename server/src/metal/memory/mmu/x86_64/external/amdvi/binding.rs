//! Physical-unit admission history, exact requester bindings and retirement.

use super::{
    Controller, Error,
    queue::{Batch, Operation, Progress},
};
use crate::metal::{
    memory::mmu::treaty::{
        BindingChange, BindingCommand, BindingControl, BindingEffect, BindingObservation,
        BindingRejection, BindingSubmission, binding::Sealed,
    },
    platform::description::acpi::Ivhd,
};
use core::convert::Infallible;
use limen_treaty::model::{PhysicalAddress, PhysicalRange};

/// Resident admission history for one described physical unit. Its owner
/// keeps this record through restarts and replacement of the controller.
pub struct Admissions {
    pub(super) scope: Scope,
    pub(super) epoch: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Scope {
    pub(super) registers: PhysicalRange,
    pub(super) segment: u16,
    pub(super) device: u16,
}

impl From<Ivhd<'_>> for Scope {
    fn from(unit: Ivhd<'_>) -> Self {
        Self {
            registers: unit.registers(),
            segment: unit.segment(),
            device: unit.device(),
        }
    }
}

impl Admissions {
    pub fn new(unit: Ivhd<'_>) -> Self {
        Self {
            scope: unit.into(),
            epoch: 0,
        }
    }

    pub(super) fn advance(&mut self, scope: Scope) -> Result<(), Error> {
        if self.scope != scope {
            return Err(Error::Window);
        }
        self.epoch = self.epoch.checked_add(1).ok_or(Error::Exhausted)?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Context {
    pub requester: u16,
    pub domain: u16,
    pub root: PhysicalAddress,
    pub levels: u8,
}

/// The exact admitted generation of a requester on one physical unit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Binding {
    pub(super) scope: Scope,
    pub(super) epoch: u64,
    pub(super) number: u64,
    pub(super) retirement: u64,
    pub(super) context: Context,
}

impl Binding {
    pub fn context(self) -> Context {
        self.context
    }
}

impl Controller<'_> {
    /// The binding is usable only after the original batch settles.
    pub fn binding(&self, requester: u16) -> Option<Binding> {
        let (index, binding) = self.find(requester)?;
        match self.progress {
            Progress::Ready => Some(binding),
            Progress::Waiting(Batch {
                operation: Operation::Bind(pending) | Operation::Unbind(pending),
                ..
            }) if pending != index => Some(binding),
            _ => None,
        }
    }

    fn find(&self, requester: u16) -> Option<(usize, Binding)> {
        self.bindings.iter().enumerate().find_map(|(index, entry)| {
            entry
                .filter(|entry| entry.context.requester == requester)
                .map(|entry| (index, entry))
        })
    }

    /// # Safety
    /// The selected requester and outstanding fabric requests are quiesced
    /// through completion. It has no aliases, ATS or interrupt remapping
    /// obligations. The complete host table remains pinned and visible
    /// through unbind and matching completion. The caller controls its
    /// nonzero domain exclusively within the physical IOMMU.
    unsafe fn begin_bind(&mut self, context: Context) -> Result<Binding, Error> {
        if !matches!(self.progress, Progress::Ready) {
            return Err(Error::Occupied);
        }
        if !self.unit.selects(context.requester) {
            return Err(Error::Unsupported);
        }
        let entry = context.device_entry()?;
        let index = admit_binding(self.bindings, context)?;
        let (number, retirement) = reserve_binding_numbers(&mut self.next)?;
        let binding = Binding {
            scope: self.admissions.scope,
            epoch: self.admissions.epoch,
            number,
            retirement,
            context,
        };
        self.bindings[index] = Some(binding);
        let dte = &mut self.storage.table.0.0[usize::from(context.requester)];
        dte[1] = entry[1];
        dte[0] = entry[0];
        self.invalidate(number, Operation::Bind(index));
        Ok(binding)
    }

    fn binding_batch(&self) -> Option<(Batch, BindingEffect<Binding, Infallible>)> {
        let batch = match self.progress {
            Progress::Waiting(batch) | Progress::Unconfirmed(Some(batch)) => batch,
            _ => return None,
        };
        let effect = match batch.operation {
            Operation::Bind(index) => BindingEffect::Bound(self.bindings[index]?),
            Operation::Unbind(index) => BindingEffect::Unbound(self.bindings[index]?),
            _ => return None,
        };
        Some((batch, effect))
    }
}

impl Sealed for Controller<'_> {}

impl BindingControl for Controller<'_> {
    type Context = Context;
    type Binding = Binding;
    type Refresh = Infallible;
    type Command = u64;
    type Error = Error;

    unsafe fn begin_binding(&mut self, context: Context) -> BindingSubmission<Self> {
        let binding = unsafe { self.begin_bind(context) }.map_err(BindingRejection::Rejected)?;
        Ok(BindingCommand::new(
            binding.number,
            BindingEffect::Bound(binding),
        ))
    }

    unsafe fn begin_refresh(&mut self, _: Binding, scope: Infallible) -> BindingSubmission<Self> {
        match scope {}
    }

    unsafe fn begin_retirement(&mut self, binding: Binding) -> BindingSubmission<Self> {
        if !matches!(self.progress, Progress::Ready) {
            return Err(BindingRejection::Rejected(Error::Occupied));
        }
        let index = binding_slot(self.bindings, binding).map_err(BindingRejection::Rejected)?;
        self.invalidate(binding.retirement, Operation::Unbind(index));
        Ok(BindingCommand::new(
            binding.retirement,
            BindingEffect::Unbound(binding),
        ))
    }

    fn unsettled_change(&self) -> Option<BindingChange<Self>> {
        let (batch, effect) = self.binding_batch()?;
        Some(BindingCommand::new(batch.number, effect))
    }

    fn observe_change(&mut self, change: BindingChange<Self>) -> BindingObservation<Self> {
        let pending = self.binding_batch();
        change.observe(
            pending.map(|(batch, effect)| BindingCommand::new(batch.number, effect)),
            |_| {
                if matches!(self.progress, Progress::Unconfirmed(_)) {
                    return Err(Error::Unconfirmed);
                }
                let (batch, effect) = pending.expect("matched binding batch");
                match self.observe_batch(batch) {
                    Ok(()) => {
                        if let Operation::Unbind(index) = batch.operation {
                            self.bindings[index] = None;
                        }
                        self.progress = Progress::Ready;
                        Ok(Some(effect))
                    }
                    Err(Error::Pending) => Ok(None),
                    Err(error) => Err(error),
                }
            },
        )
    }
}

pub(super) fn reserve_binding_numbers(next: &mut u64) -> Result<(u64, u64), Error> {
    // Allocate the exact completion stamps for both transitions before any
    // table write. A later admission cannot exhaust this binding's teardown.
    let retirement = next.checked_add(2).ok_or(Error::Exhausted)?;
    *next = retirement;
    Ok((retirement - 1, retirement))
}

pub(super) fn binding_slot(
    bindings: &[Option<Binding>],
    expected: Binding,
) -> Result<usize, Error> {
    bindings
        .iter()
        .position(|entry| *entry == Some(expected))
        .ok_or(Error::Stale)
}

pub(super) fn admit_binding(
    bindings: &[Option<Binding>],
    context: Context,
) -> Result<usize, Error> {
    let mut vacant = None;
    for (index, entry) in bindings.iter().enumerate() {
        match entry {
            Some(entry)
                if entry.context.requester == context.requester
                    || entry.context.domain == context.domain =>
            {
                return Err(Error::Occupied);
            }
            None if vacant.is_none() => vacant = Some(index),
            _ => {}
        }
    }
    vacant.ok_or(Error::Exhausted)
}
