//! QEMU virt board profile: identity-mapped PL011 and PSCI over HVC.
use crate::metal::console::Services;
use core::arch::asm;

pub(super) fn services() -> Services {
    Services {
        output: Some(write_byte),
        reset: Some(reset),
    }
}

fn write_byte(byte: u8) -> bool {
    let uart = core::ptr::without_provenance_mut::<u32>(0x0900_0000);
    // Avoid blocking failure shutdown forever on a stalled UART.
    for _ in 0..65536 {
        if unsafe { uart.wrapping_add(6).read_volatile() } & (1 << 5) == 0 {
            unsafe { uart.write_volatile(u32::from(byte)) };
            return true;
        }
    }
    false
}

fn reset(_: bool) {
    // PSCI SYSTEM_OFF has no success or failure reason field.
    unsafe {
        asm!("hvc #0", inout("x0") 0x8400_0008usize => _,
             clobber_abi("C"), options(nostack));
    }
}
