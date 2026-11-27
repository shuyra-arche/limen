//! Physical-unit admission history, exact requester bindings and retirement.

use super::{
    Controller, Error,
    queue::{Batch, Operation, Progress},
};
use crate::metal::{
    memory::mmu::treaty::{Retirement, Unbind, retirement::Sealed},
    platform::description::acpi::Ivhd,
};
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

/// Observation of the completion store reserved by this original binding.
pub struct Unbinding {
    binding: Binding,
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
    pub unsafe fn bind(&mut self, context: Context) -> Result<(), Error> {
        if !matches!(self.progress, Progress::Ready) {
            return Err(Error::Occupied);
        }
        if !self.unit.selects(context.requester) {
            return Err(Error::Unsupported);
        }
        let entry = context.device_entry()?;
        let index = admit_binding(self.bindings, context)?;
        let (number, retirement) = reserve_binding_numbers(&mut self.next)?;
        self.bindings[index] = Some(Binding {
            scope: self.admissions.scope,
            epoch: self.admissions.epoch,
            number,
            retirement,
            context,
        });
        let dte = &mut self.storage.table.0.0[usize::from(context.requester)];
        dte[1] = entry[1];
        dte[0] = entry[0];
        self.invalidate(number, Operation::Bind(index));
        self.poll()
    }

    fn retirement_batch(&self, binding: Binding) -> Option<(Batch, usize)> {
        let batch = match self.progress {
            Progress::Waiting(batch) | Progress::Unconfirmed(Some(batch)) => batch,
            _ => return None,
        };
        let Operation::Unbind(index) = batch.operation else {
            return None;
        };
        (batch.number == binding.retirement && self.bindings[index] == Some(binding))
            .then_some((batch, index))
    }
}

impl Sealed for Controller<'_> {}

impl Unbind for Controller<'_> {
    type Binding = Binding;
    type Ticket = Unbinding;
    type Error = Error;

    unsafe fn retire(&mut self, binding: Binding) -> Result<Unbinding, Error> {
        if !matches!(self.progress, Progress::Ready) {
            return Err(Error::Occupied);
        }
        let index = binding_slot(self.bindings, binding)?;
        self.invalidate(binding.retirement, Operation::Unbind(index));
        Ok(Unbinding { binding })
    }

    fn pending_retirement(&self, binding: Binding) -> Option<Unbinding> {
        self.retirement_batch(binding)
            .map(|_| Unbinding { binding })
    }

    fn observe_retirement(&mut self, ticket: &Unbinding) -> Result<Retirement, Error> {
        let (batch, index) = self.retirement_batch(ticket.binding).ok_or(Error::Stale)?;
        if matches!(self.progress, Progress::Unconfirmed(_)) {
            return Err(Error::Unconfirmed);
        }
        match self.observe_batch(batch) {
            Ok(()) => {
                self.bindings[index] = None;
                self.progress = Progress::Ready;
                Ok(Retirement::Settled)
            }
            Err(Error::Pending) => Ok(Retirement::Pending),
            Err(error) => Err(error),
        }
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
