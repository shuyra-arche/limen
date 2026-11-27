//! Requester identity and the sequence retained by its original controller.

use super::{Error, directory::Context};
use limen_treaty::model::{PhysicalAddress, PhysicalRange};

/// Sequence resident with the original physical register scope. The
/// composition owner preserves it across reset and re-admission.
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
            return Err(Error::Address);
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

/// Identifies one installed generation. A copied value does not keep the
/// binding live and grants no access to its translation tables.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Binding {
    pub(super) controller: PhysicalAddress,
    pub(super) generation: u64,
    pub(super) context: Context,
}

impl Binding {
    pub(super) fn new(controller: PhysicalAddress, generation: u64, context: Context) -> Self {
        Self {
            controller,
            generation,
            context,
        }
    }

    pub fn context(self) -> Context {
        self.context
    }
}

#[cfg(test)]
#[path = "../../../../../../../tests/metal/memory/mmu/riscv64/external/iommu/binding.rs"]
mod tests;
