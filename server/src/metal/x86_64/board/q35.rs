//! QEMU q35 diagnostic profile with debugcon and isa-debug-exit.
use crate::metal::console::Services;
use core::arch::asm;

pub(super) fn services() -> Services {
    Services {
        output: Some(write_byte),
        reset: Some(reset),
    }
}

fn write_byte(byte: u8) -> bool {
    unsafe { asm!("out dx, al", in("dx") 0xe9u16, in("al") byte, options(nostack)) };
    true
}

fn reset(success: bool) {
    let code: u32 = if success { 0x10 } else { 0x11 };
    unsafe { asm!("out dx, eax", in("dx") 0xf4u16, in("eax") code, options(nostack)) };
}
