//! Firmware calls required by the trusted boot prefix.

use crate::metal::console::Services;
use core::arch::asm;

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
fn reset(success: bool) {
    // Shutdown with no reason (0) or system failure (1).
    let _ = call(SYSTEM_RESET, 0, 0, usize::from(!success), 0);
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

pub(crate) fn services() -> Services {
    let available = extensions();
    Services {
        output: available.console.then_some(write_byte),
        reset: available.reset.then_some(reset),
    }
}
