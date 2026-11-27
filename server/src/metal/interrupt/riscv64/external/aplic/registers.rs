//! Ordered access to the domain's shared words and the actual IDC aperture.

use super::{
    super::Barrier,
    SOURCE_LIMIT,
    encoding::{DomainConfiguration, SourceConfiguration},
};
use crate::metal::memory::access::treaty::device::DeviceWindow;

const DOMAIN: usize = 0;
const PENDING: usize = 0x1c00;
const INPUT: usize = 0x1d00;
const ENABLE: usize = 0x1e00;
const SET_ENABLE: usize = 0x1edc;
const CLEAR_ENABLE: usize = 0x1fdc;
const TARGET: usize = 0x3000;

pub(super) struct Words<'a> {
    mapping: DeviceWindow<'a>,
    barrier: Barrier,
}

impl<'a> Words<'a> {
    pub(super) fn new(mapping: DeviceWindow<'a>, barrier: Barrier) -> Self {
        Self { mapping, barrier }
    }

    pub(super) fn read(&mut self, offset: usize) -> u32 {
        self.barrier.synchronize();
        let value = unsafe { self.mapping.read_u32(offset) }.expect("checked APLIC word");
        self.barrier.synchronize();
        value
    }

    pub(super) fn write(&mut self, offset: usize, value: u32) {
        unsafe { self.mapping.write_u32(offset, value) }.expect("checked APLIC word");
        self.barrier.synchronize();
    }
}

pub(super) struct Registers<'a>(pub(super) Words<'a>);

impl Registers<'_> {
    pub(super) fn confirms(&mut self, configuration: DomainConfiguration) -> bool {
        configuration.confirms(self.0.read(DOMAIN))
    }

    pub(super) fn configure(&mut self, configuration: DomainConfiguration) {
        self.0.write(DOMAIN, configuration.write_bits());
    }

    pub(super) fn source(&mut self, source: u32) -> u32 {
        self.0.read(source_offset(source))
    }

    pub(super) fn set_source(&mut self, source: u32, configuration: SourceConfiguration) {
        self.0.write(source_offset(source), configuration.bits());
    }

    pub(super) fn target(&mut self, source: u32) -> u32 {
        self.0.read(TARGET + source_offset(source))
    }

    pub(super) fn set_target(&mut self, source: u32, bits: u32) {
        self.0.write(TARGET + source_offset(source), bits);
    }

    pub(super) fn set_enabled(&mut self, source: u32, enabled: bool) {
        self.0
            .write(if enabled { SET_ENABLE } else { CLEAR_ENABLE }, source);
    }

    pub(super) fn enabled(&mut self, source: u32) -> bool {
        self.bit(ENABLE, source)
    }

    pub(super) fn pending(&mut self, source: u32) -> bool {
        self.bit(PENDING, source)
    }

    pub(super) fn input(&mut self, source: u32) -> bool {
        self.bit(INPUT, source)
    }

    fn bit(&mut self, register: usize, source: u32) -> bool {
        self.0.read(register + (source as usize / 32) * 4) & (1 << (source % 32)) != 0
    }
}

fn source_offset(source: u32) -> usize {
    debug_assert!((1..=SOURCE_LIMIT).contains(&source));
    source as usize * 4
}
