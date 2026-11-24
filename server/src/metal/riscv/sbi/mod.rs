//! Firmware calls required by the trusted boot prefix.

use core::arch::asm;
use core::fmt::{self, Write};

mod probe;
pub(crate) use probe::Extensions;
use probe::{DEBUG_CONSOLE, SYSTEM_RESET};

const BASE: usize = 0x10;
const HART_STATE_MANAGEMENT: usize = 0x48534d;
pub(super) const ALREADY_AVAILABLE: isize = -6;

pub(crate) fn extensions() -> Extensions {
    Extensions::probe(|function, extension| call(BASE, function, extension, 0, 0))
}

fn write_byte(byte: u8) -> bool {
    call(DEBUG_CONSOLE, 2, usize::from(byte), 0, 0).0 == 0
}

/// End the trusted boot path through SBI system reset.
pub fn shutdown(success: bool) -> ! {
    // Shutdown with no reason (0) or system failure (1).
    let _ = call(SYSTEM_RESET, 0, 0, usize::from(!success), 0);
    super::boot::park()
}

pub(super) fn start_hart(hart: usize, address: usize, opaque: usize) -> Result<(), isize> {
    match call(HART_STATE_MANAGEMENT, 0, hart, address, opaque).0 {
        0 => Ok(()),
        error => Err(error),
    }
}

fn call(
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

pub(super) fn reject(firmware: Extensions, message: fmt::Arguments<'_>) -> ! {
    if firmware.console {
        let _ = Console.write_fmt(message);
    }
    if firmware.reset {
        shutdown(false);
    }
    super::boot::park()
}

/// Write an early diagnostic to the firmware debug console.
pub fn write(message: &str) {
    let _ = Console.write_str(message);
}

struct Console;

impl Write for Console {
    fn write_str(&mut self, message: &str) -> fmt::Result {
        if message.bytes().all(write_byte) {
            Ok(())
        } else {
            Err(fmt::Error)
        }
    }
}
