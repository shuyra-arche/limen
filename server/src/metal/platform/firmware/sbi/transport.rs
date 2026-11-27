//! Firmware calls required by the trusted boot prefix.

use core::arch::asm;

use super::probe::{DEBUG_CONSOLE, Extensions, SYSTEM_RESET};

pub(in crate::metal::platform::firmware) const BASE: usize = 0x10;

pub(crate) fn extensions() -> Extensions {
    Extensions::probe(|function, extension| call(BASE, function, extension, 0, 0))
}

pub(crate) fn write_byte(byte: u8) -> bool {
    call(DEBUG_CONSOLE, 2, usize::from(byte), 0, 0).0 == 0
}

/// End the trusted boot path through SBI system reset.
pub(crate) fn reset(success: bool) {
    // Shutdown with no reason (0) or system failure (1).
    let _ = call(SYSTEM_RESET, 0, 0, usize::from(!success), 0);
}

pub(in crate::metal::platform::firmware) fn call(
    extension: usize,
    function: usize,
    first: usize,
    second: usize,
    third: usize,
) -> (isize, usize) {
    let (error, value);
    unsafe {
        asm!(
            "ecall",
            inlateout("a0") first => error,
            inlateout("a1") second => value,
            in("a2") third,
            in("a6") function,
            in("a7") extension,
            options(nostack),
        );
    }
    (error, value)
}
