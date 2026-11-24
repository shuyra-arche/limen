//! Firmware calls required by the trusted boot prefix.

use core::arch::asm;

mod probe;
pub(crate) use probe::Extensions;
use probe::{DEBUG_CONSOLE, SYSTEM_RESET};

const BASE: usize = 0x10;

pub(crate) fn extensions() -> Extensions {
    Extensions::probe(|function, extension| call(BASE, function, extension, 0))
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
