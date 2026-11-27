//! Single-level Device Directory Table and its requester ownership.

use super::{DeviceOrder, Error, Profile};
use limen_treaty::model::{PhysicalAddress, PhysicalRange};

#[repr(C, align(4096))]
pub struct Page([u64; 512]);

impl Page {
    pub const EMPTY: Self = Self([0; 512]);
}

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Context {
    pub device: u32,
    pub root: PhysicalAddress,
    pub pscid: u32,
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
    pub fn context(self) -> Context {
        self.context
    }
}

pub struct Storage<'a> {
    pub page: &'static mut Page,
    pub physical: PhysicalAddress,
    pub bindings: &'a mut [Option<Binding>],
    pub sequence: &'a mut Sequence,
}

impl Storage<'_> {
    pub(super) fn validate(&self, profile: Profile) -> Result<(), Error> {
        if self.bindings.is_empty() || self.bindings.len() > profile.directory_capacity() {
            return Err(Error::Capacity);
        }
        if self.bindings.iter().any(Option::is_some) || self.page.0.iter().any(|&word| word != 0) {
            return Err(Error::Mode);
        }
        if !profile.sv39 || !profile.little_endian_tables {
            return Err(Error::Mode);
        }
        Ok(())
    }

    pub(super) fn binding(&self, device: u32) -> Option<Binding> {
        self.bindings
            .iter()
            .flatten()
            .find(|binding| binding.context.device == device)
            .copied()
    }

    pub(super) fn occupied(&self) -> bool {
        self.bindings.iter().any(Option::is_some)
    }

    pub(super) fn entry(
        &self,
        profile: Profile,
        context: Context,
    ) -> Result<(usize, [u64; 4]), Error> {
        let range = profile.context_words(context.device)?;
        // Only one configuration may occupy a DID. An intentional alias
        // can share a PSCID only if it reaches the exact same table root.
        if self.bindings.iter().flatten().any(|binding| {
            binding.context.device == context.device
                || (binding.context.pscid == context.pscid && binding.context.root != context.root)
        }) {
            return Err(Error::Mode);
        }
        let slot = self
            .bindings
            .iter()
            .position(Option::is_none)
            .ok_or(Error::Capacity)?;
        if self.page.0[range].iter().any(|&word| word != 0) {
            return Err(Error::Unconfirmed);
        }
        Ok((slot, profile.context(context.root, context.pscid)?))
    }

    pub(super) fn publish(
        &mut self,
        profile: Profile,
        slot: usize,
        binding: Binding,
        words: [u64; 4],
        order: &DeviceOrder,
    ) {
        let start = profile
            .context_words(binding.context.device)
            .expect("validated entry")
            .start;
        self.bindings[slot] = Some(binding);
        for (offset, word) in words.into_iter().enumerate().skip(1) {
            unsafe { (&raw mut self.page.0[start + offset]).write_volatile(word.to_le()) };
        }
        order.fence();
        unsafe { (&raw mut self.page.0[start]).write_volatile(words[0].to_le()) };
        order.fence();
    }

    pub(super) fn block(&mut self, profile: Profile, binding: Binding, order: &DeviceOrder) {
        let start = profile
            .context_words(binding.context.device)
            .expect("validated entry")
            .start;
        unsafe { (&raw mut self.page.0[start]).write_volatile(0) };
        order.fence();
    }

    pub(super) fn retire(&mut self, profile: Profile, binding: Binding, order: &DeviceOrder) {
        let entry = profile
            .context_words(binding.context.device)
            .expect("validated entry");
        for word in entry {
            unsafe { (&raw mut self.page.0[word]).write_volatile(0) };
        }
        order.fence();
        let record = self
            .bindings
            .iter_mut()
            .find(|record| **record == Some(binding));
        *record.expect("retained original binding") = None;
    }
}

#[cfg(test)]
#[path = "../../../../../../../tests/metal/memory/mmu/riscv64/external/iommu/directory.rs"]
mod tests;
