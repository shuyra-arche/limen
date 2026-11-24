//! QEMU virt board profile: identity-mapped PL011 and PSCI over HVC.
use crate::metal::aarch64::{firmware::psci::reset, pl011};
use crate::metal::console::Services;

pub(in crate::entry) fn services() -> Services {
    Services {
        output: Some(write_byte),
        reset: Some(reset),
    }
}

fn write_byte(byte: u8) -> bool {
    unsafe { pl011::write_byte(0x0900_0000, byte) }
}
