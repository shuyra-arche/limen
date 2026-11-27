//! Linear stream-table storage and stream binding ownership.

use super::{DeviceOrder, Error, translation::Format};
use limen_treaty::model::{PhysicalAddress, PhysicalRange};

#[repr(C, align(64))]
pub struct Entry([u64; 8]);

impl Entry {
    pub const EMPTY: Self = Self([0; 8]);
}

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
pub struct Context {
    pub stream: u32,
    pub root: PhysicalAddress,
    pub vmid: u8,
}

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
    pub entries: &'static mut [Entry],
    pub physical: PhysicalAddress,
    pub bindings: &'a mut [Option<Binding>],
    pub sequence: &'a mut Sequence,
}

impl Storage<'_> {
    pub(super) fn geometry(&self, sid_bits: u32) -> Result<u8, Error> {
        let count = self.entries.len();
        // This profile supports a linear 16-bit stream index and an 8-bit VMID.
        let bits = count.trailing_zeros();
        if !count.is_power_of_two() || bits > sid_bits.min(16) {
            return Err(Error::Rejected);
        }
        let bytes = count as u64 * 64;
        let address = self.physical.get();
        if !address.is_multiple_of(bytes) || address >> 52 != 0 {
            return Err(Error::Window);
        }
        Ok(bits as u8)
    }

    pub(super) fn validate(&self) -> Result<(), Error> {
        if self.bindings.is_empty() || self.bindings.len() > self.entries.len().min(256) {
            return Err(Error::Capacity);
        }
        if self.bindings.iter().any(Option::is_some)
            || self.entries.iter().any(|entry| entry.0 != [0; 8])
        {
            return Err(Error::Active);
        }
        Ok(())
    }

    pub(super) fn binding(&self, stream: u32) -> Option<Binding> {
        self.bindings
            .iter()
            .flatten()
            .find(|binding| binding.context.stream == stream)
            .copied()
    }

    pub(super) fn occupied(&self) -> bool {
        self.bindings.iter().any(Option::is_some)
    }

    pub(super) fn entry(
        &self,
        format: Format,
        context: Context,
    ) -> Result<(usize, [u64; 8]), Error> {
        let entry = self
            .entries
            .get(context.stream as usize)
            .ok_or(Error::Rejected)?;
        if self.bindings.iter().flatten().any(|binding| {
            binding.context.stream == context.stream || binding.context.vmid == context.vmid
        }) {
            return Err(Error::Active);
        }
        if entry.0 != [0; 8] {
            return Err(Error::Unconfirmed);
        }
        let slot = self
            .bindings
            .iter()
            .position(Option::is_none)
            .ok_or(Error::Capacity)?;
        Ok((slot, format.stream(context.root, context.vmid)?))
    }

    pub(super) fn publish(
        &mut self,
        slot: usize,
        binding: Binding,
        words: [u64; 8],
        order: &DeviceOrder,
    ) {
        let entry = &mut self.entries[binding.context.stream as usize].0;
        self.bindings[slot] = Some(binding);
        for (offset, word) in words.into_iter().enumerate().skip(1) {
            unsafe { (&raw mut entry[offset]).write_volatile(word.to_le()) };
        }
        order.fence();
        unsafe { (&raw mut entry[0]).write_volatile(words[0].to_le()) };
        order.fence();
    }

    pub(super) fn block(&mut self, binding: Binding, order: &DeviceOrder) {
        let entry = &mut self.entries[binding.context.stream as usize].0;
        unsafe { (&raw mut entry[0]).write_volatile(0) };
        order.fence();
    }

    pub(super) fn retire(&mut self, binding: Binding, order: &DeviceOrder) {
        let entry = &mut self.entries[binding.context.stream as usize].0;
        for word in entry {
            unsafe { (word as *mut u64).write_volatile(0) };
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
#[path = "../../../../../../../tests/metal/memory/mmu/aarch64/external/smmu/directory.rs"]
mod tests;
