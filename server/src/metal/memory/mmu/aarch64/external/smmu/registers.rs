//! The little-endian SMMUv3 Non-secure register file.

use super::Error;
use crate::metal::memory::access::treaty::device::DeviceWindow;
use limen_treaty::model::PhysicalRange;

pub(super) struct Register(usize);
pub(super) struct BaseRegister(usize);

pub(super) const IDR0: Register = Register(0);
pub(super) const IDR1: Register = Register(4);
pub(super) const IDR5: Register = Register(0x14);
pub(super) const CR0: Register = Register(0x20);
pub(super) const CR0_ACK: Register = Register(0x24);
pub(super) const GBPA: Register = Register(0x44);
pub(super) const GLOBAL_ERROR: Register = Register(0x60);
pub(super) const GLOBAL_ERROR_ACK: Register = Register(0x64);
pub(super) const STREAM_TABLE: BaseRegister = BaseRegister(0x80);
pub(super) const STREAM_CONFIG: Register = Register(0x88);
pub(super) const QUEUE_BASE: BaseRegister = BaseRegister(0x90);
pub(super) const QUEUE_PRODUCER: Register = Register(0x98);
pub(super) const QUEUE_CONSUMER: Register = Register(0x9c);

pub(super) const SMMU_ENABLE: u32 = 1;
pub(super) const ABORT: u32 = 1 << 20;
pub(super) const UPDATE: u32 = 1 << 31;

pub(super) struct Registers<'a>(DeviceWindow<'a>);

impl<'a> Registers<'a> {
    /// # Safety
    /// The window must map the actual Non-secure register file of the interface
    /// exclusively controlled by this controller.
    pub(super) unsafe fn new(window: &'a mut DeviceWindow<'_>) -> Self {
        Self(window.reborrow())
    }

    pub(super) fn physical(&self) -> PhysicalRange {
        self.0.physical()
    }

    pub(super) fn supports(&self, register: Register) -> bool {
        self.0.supports_u32(register.0)
    }

    pub(super) fn supports_base(&self, register: BaseRegister) -> bool {
        self.0.supports_u64(register.0)
    }

    pub(super) fn read(&mut self, register: Register) -> Result<u32, Error> {
        unsafe { self.0.read_u32(register.0) }
            .map(u32::from_le)
            .map_err(|_| Error::Window)
    }

    pub(super) fn write(&mut self, register: Register, value: u32) -> Result<(), Error> {
        unsafe { self.0.write_u32(register.0, value.to_le()) }.map_err(|_| Error::Window)
    }

    pub(super) fn read_base(&mut self, register: BaseRegister) -> Result<u64, Error> {
        unsafe { self.0.read_u64(register.0) }
            .map(u64::from_le)
            .map_err(|_| Error::Window)
    }

    pub(super) fn write_base(&mut self, register: BaseRegister, value: u64) -> Result<(), Error> {
        unsafe { self.0.write_u64(register.0, value.to_le()) }.map_err(|_| Error::Window)
    }
}
