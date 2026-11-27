//! Layout, validity and retention of the original linear stream table.

use super::{DeviceOrder, Error};
use limen_treaty::model::PhysicalAddress;

#[repr(C, align(64))]
pub struct Entry([u64; 8]);

impl Entry {
    pub const EMPTY: Self = Self([0; 8]);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamTable {
    pub base: PhysicalAddress,
    pub bits: u8,
}

pub(super) struct Directory {
    entries: &'static mut [Entry],
    table: StreamTable,
}

impl Directory {
    pub(super) fn new(
        entries: &'static mut [Entry],
        base: PhysicalAddress,
        sid_bits: u32,
    ) -> Result<Self, Error> {
        let count = entries.len();
        // The profile supports a linear 16-bit stream index.
        let bits = count.trailing_zeros();
        if !count.is_power_of_two() || bits > sid_bits.min(16) {
            return Err(Error::Rejected);
        }
        let bytes = count as u64 * size_of::<Entry>() as u64;
        let address = base.get();
        if !address.is_multiple_of(bytes) || address >> 52 != 0 {
            return Err(Error::Window);
        }
        let table = StreamTable {
            base,
            bits: bits as u8,
        };
        Ok(Self { entries, table })
    }

    pub(super) fn table(&self) -> StreamTable {
        self.table
    }

    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(super) fn empty(&self) -> bool {
        self.entries.iter().all(Entry::empty)
    }

    pub(super) fn entry(&self, stream: u32) -> Result<&Entry, Error> {
        self.entries.get(stream as usize).ok_or(Error::Rejected)
    }

    pub(super) fn publish(&mut self, stream: u32, words: [u64; 8], order: &DeviceOrder) {
        let entry = &mut self.entries[stream as usize].0;
        for (offset, word) in words.into_iter().enumerate().skip(1) {
            unsafe { (&raw mut entry[offset]).write_volatile(word.to_le()) };
        }
        order.fence();
        unsafe { (&raw mut entry[0]).write_volatile(words[0].to_le()) };
        order.fence();
    }

    pub(super) fn block(&mut self, stream: u32, order: &DeviceOrder) {
        let entry = &mut self.entries[stream as usize].0;
        unsafe { (&raw mut entry[0]).write_volatile(0) };
        order.fence();
    }

    pub(super) fn retire(&mut self, stream: u32, order: &DeviceOrder) {
        for word in &mut self.entries[stream as usize].0 {
            unsafe { (word as *mut u64).write_volatile(0) };
        }
        order.fence();
    }
}

impl Entry {
    pub(super) fn empty(&self) -> bool {
        self.0 == [0; 8]
    }
}
