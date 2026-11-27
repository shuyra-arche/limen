//! One selector/data pair for the original I/O APIC mapping.

use crate::metal::memory::access::treaty::device::DeviceWindow;
use limen_treaty::model::PhysicalAddress;

const SELECT: usize = 0;
const DATA: usize = 0x10;

pub(super) struct Port<'a>(DeviceWindow<'a>);

impl<'map> Port<'map> {
    pub(super) fn from_window(window: &'map mut DeviceWindow<'_>) -> Option<Self> {
        (window.physical().start().get().is_multiple_of(4096)
            && window.supports_u32(SELECT)
            && window.supports_u32(DATA))
        .then(|| Self(window.reborrow()))
    }

    pub(super) fn address(&self) -> PhysicalAddress {
        self.0.physical().start()
    }

    fn select(&mut self, register: u8) {
        // The caller excludes every other user of this shared selector/window pair.
        unsafe { self.0.write_u32(SELECT, u32::from(register)) }.expect("I/O APIC selector");
    }

    pub(super) fn read(&mut self, register: u8) -> u32 {
        self.select(register);
        unsafe { self.0.read_u32(DATA) }.expect("I/O APIC register")
    }

    pub(super) fn write(&mut self, register: u8, value: u32) {
        self.select(register);
        unsafe { self.0.write_u32(DATA, value) }.expect("I/O APIC register");
    }
}
