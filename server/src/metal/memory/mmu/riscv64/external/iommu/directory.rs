//! Single-level Device Directory Table and its requester ownership.

pub use super::binding::{Binding, Sequence};
use super::{
    DeviceOrder, Error, Profile,
    registers::{DirectoryWord, Registers},
};
use core::ops::Range;
use limen_treaty::model::PhysicalAddress;

#[repr(C, align(4096))]
pub struct Page([u64; PAGE_WORDS]);

const PAGE_WORDS: usize = 512;

impl Page {
    pub const EMPTY: Self = Self([0; PAGE_WORDS]);

    const fn stride(profile: Profile) -> usize {
        if profile.extended_context { 8 } else { 4 }
    }

    const fn capacity(profile: Profile) -> usize {
        PAGE_WORDS / Self::stride(profile)
    }

    /// Locate a device context within a single-level Device Directory Table.
    fn context_words(profile: Profile, device: u32) -> Result<Range<usize>, Error> {
        let index = usize::try_from(device).map_err(|_| Error::Address)?;
        if index >= Self::capacity(profile) {
            return Err(Error::Address);
        }
        let words = Self::stride(profile);
        Ok(index * words..(index + 1) * words)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Context {
    pub device: u32,
    pub root: PhysicalAddress,
    pub pscid: u32,
}

impl Context {
    /// Encode the basic context. The directory leaves every optional MSI
    /// field zero and publishes the valid word after the other fields.
    fn words(self, profile: Profile) -> Result<[u64; 4], Error> {
        if self.pscid >= 1 << 20 {
            return Err(Error::Address);
        }
        Ok([
            1,
            0,
            u64::from(self.pscid) << 12,
            profile.first_stage(self.root)?,
        ])
    }
}

pub struct Storage<'a> {
    pub page: &'static mut Page,
    pub physical: PhysicalAddress,
    pub bindings: &'a mut [Option<Binding>],
    pub sequence: &'a mut Sequence,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Directory {
    Off,
    Bare,
    One,
    Two,
    Three,
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

pub(super) struct Entry {
    slot: usize,
    start: usize,
    words: [u64; 4],
}

/// Owns the backing directory, its installed selection, and the exact
/// lifetime of entries until each requester's retirement settles.
pub(super) struct Table<'a> {
    page: &'static mut Page,
    physical: PhysicalAddress,
    bindings: &'a mut [Option<Binding>],
    selection: Selection,
    retained_root: Option<PhysicalAddress>,
}

impl<'a> Table<'a> {
    pub(super) fn admit(
        page: &'static mut Page,
        physical: PhysicalAddress,
        bindings: &'a mut [Option<Binding>],
        profile: Profile,
        initial: DirectoryWord,
    ) -> Result<Self, Error> {
        if !matches!(initial.mode(), Directory::Off | Directory::Bare) {
            // There is no original storage or settlement evidence to take
            // over an inherited live directory.
            return Err(Error::Mode);
        }
        let table = Self {
            page,
            physical,
            bindings,
            selection: Selection::Selected(initial),
            retained_root: None,
        };
        table.validate(profile)?;
        initial.one(table.physical, profile.address_bits)?;
        Ok(table)
    }

    pub(super) fn last_confirmed_mode(&self) -> Directory {
        match self.selection {
            Selection::Selected(word)
            | Selection::Pending { original: word, .. }
            | Selection::Unconfirmed { original: word, .. } => word.mode(),
        }
    }

    pub(super) fn selected_mode(&self) -> Result<Directory, Error> {
        self.selected().map(DirectoryWord::mode)
    }

    pub(super) fn retained_root(&self) -> Option<PhysicalAddress> {
        self.retained_root
    }

    pub(super) fn uncertain_roots(&self) -> Option<[Option<PhysicalAddress>; 2]> {
        match self.selection {
            Selection::Unconfirmed { original, target } => Some([original.root(), target.root()]),
            _ => None,
        }
    }

    fn selected(&self) -> Result<DirectoryWord, Error> {
        match self.selection {
            Selection::Selected(word) => Ok(word),
            Selection::Pending { .. } => Err(Error::Busy),
            Selection::Unconfirmed { .. } => Err(Error::Unconfirmed),
        }
    }

    fn issue_directory(
        &mut self,
        original: DirectoryWord,
        target: DirectoryWord,
        registers: &mut Registers<'_>,
    ) -> Result<(), Error> {
        self.selection = Selection::Pending { original, target };
        registers.write_directory(target)
    }

    pub(super) fn begin_shut(&mut self, registers: &mut Registers<'_>) -> Result<(), Error> {
        let original = self.selected()?;
        if self.occupied() {
            return Err(Error::Mode);
        }
        self.issue_directory(original, original.off(), registers)
    }

    pub(super) fn begin_install(&mut self, registers: &mut Registers<'_>) -> Result<(), Error> {
        let original = self.selected()?;
        if original.mode() != Directory::Off || self.retained_root.is_some() {
            return Err(Error::Mode);
        }
        let target = original.one(self.physical, registers.address_bits())?;
        self.issue_directory(original, target, registers)
    }

    /// Observe only the original register write. Delayed readback never
    /// resubmits it, and a contradictory readback blocks further programming.
    pub(super) fn observe(&mut self, registers: &mut Registers<'_>) -> Result<Directory, Error> {
        let Selection::Pending { original, target } = self.selection else {
            self.selected()?;
            return Err(Error::Mode);
        };
        let actual = match registers.observe_directory() {
            Err(Error::Mode) => {
                self.selection = Selection::Unconfirmed { original, target };
                return Err(Error::Unconfirmed);
            }
            result => result?,
        };
        if !actual.confirms(target) {
            self.selection = Selection::Unconfirmed { original, target };
            return Err(Error::Unconfirmed);
        }
        if actual.mode() == Directory::Off
            && let Some(root) = original.root()
        {
            self.retained_root = Some(root);
        }
        self.selection = Selection::Selected(actual);
        Ok(actual.mode())
    }

    fn validate(&self, profile: Profile) -> Result<(), Error> {
        if self.bindings.is_empty() || self.bindings.len() > Page::capacity(profile) {
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

    pub(super) fn entry(&self, profile: Profile, context: Context) -> Result<Entry, Error> {
        let range = Page::context_words(profile, context.device)?;
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
        let start = range.start;
        if self.page.0[range].iter().any(|&word| word != 0) {
            return Err(Error::Unconfirmed);
        }
        Ok(Entry {
            slot,
            start,
            words: context.words(profile)?,
        })
    }

    pub(super) fn publish(&mut self, entry: Entry, binding: Binding, order: &DeviceOrder) {
        let Entry { slot, start, words } = entry;
        self.bindings[slot] = Some(binding);
        for (offset, word) in words.into_iter().enumerate().skip(1) {
            unsafe { (&raw mut self.page.0[start + offset]).write_volatile(word.to_le()) };
        }
        order.fence();
        unsafe { (&raw mut self.page.0[start]).write_volatile(words[0].to_le()) };
        order.fence();
    }

    pub(super) fn block(&mut self, profile: Profile, binding: Binding, order: &DeviceOrder) {
        let start = Page::context_words(profile, binding.context.device)
            .expect("validated entry")
            .start;
        unsafe { (&raw mut self.page.0[start]).write_volatile(0) };
        order.fence();
    }

    pub(super) fn retire(&mut self, profile: Profile, binding: Binding, order: &DeviceOrder) {
        let entry = Page::context_words(profile, binding.context.device).expect("validated entry");
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
