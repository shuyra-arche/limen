//! Firmware calls required by the trusted boot prefix.

use core::arch::asm;

const BASE: usize = 0x10;
const DEBUG_CONSOLE: usize = 0x4442_434e;
const SYSTEM_RESET: usize = 0x5352_5354;

pub(crate) fn available() -> bool {
    let (error, version) = call(BASE, 0, 0, 0);
    error == 0 && version >= 2
}

pub(crate) fn write_byte(byte: u8) -> bool {
    call(DEBUG_CONSOLE, 2, usize::from(byte), 0).0 == 0
}

pub(crate) fn shutdown(success: bool) -> ! {
    // Shutdown with no reason (0) or system failure (1).
    let _ = call(SYSTEM_RESET, 0, 0, usize::from(!success));
    super::boot::park()
}

fn call(extension: usize, function: usize, first: usize, second: usize) -> (isize, usize) {
    let (error, value);
    unsafe {
        asm!(
            "ecall",
            inlateout("a0") first => error,
            inlateout("a1") second => value,
            in("a6") function,
            in("a7") extension,
            options(nostack),
        );
    }
    (error, value)
}
