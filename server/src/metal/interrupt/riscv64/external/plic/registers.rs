//! PLIC register coordinates and ordered access to the admitted window.

use super::{Access, Failure};

pub(super) const SOURCES: u32 = 1023;
pub(super) const CONTEXTS: usize = 15872;
const ENABLE: usize = 0x2000;
const ENABLE_STRIDE: usize = 128;
const CONTEXT: usize = 0x20_0000;
const CONTEXT_STRIDE: usize = 4096;

pub(super) fn threshold(context: usize) -> usize {
    CONTEXT + context * CONTEXT_STRIDE
}

pub(super) fn enables(context: usize) -> usize {
    ENABLE + context * ENABLE_STRIDE
}

pub(super) fn priority(source: u32) -> usize {
    source as usize * 4
}

pub(super) fn enable(context: usize, source: u32) -> (usize, u32) {
    (
        enables(context) + source as usize / 32 * 4,
        1 << (source % 32),
    )
}

pub(super) fn claim(context: usize) -> usize {
    threshold(context) + 4
}

impl Access<'_, '_, '_> {
    pub(super) fn read(&mut self, offset: usize) -> u32 {
        self.barrier.synchronize();
        let bits = unsafe { self.mapping.read_u32(offset) }.expect("admitted PLIC window");
        self.barrier.synchronize();
        bits
    }

    pub(super) fn write(&mut self, offset: usize, value: u32) {
        self.barrier.synchronize();
        unsafe { self.mapping.write_u32(offset, value) }.expect("admitted PLIC window");
        self.barrier.synchronize();
    }

    pub(super) fn program(&mut self, offset: usize, value: u32) -> Result<(), Failure> {
        self.write(offset, value);
        (self.read(offset) == value)
            .then_some(())
            .ok_or(Failure::Unconfirmed)
    }
}
