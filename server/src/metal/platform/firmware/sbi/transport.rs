//! Firmware calls required by the trusted boot prefix.

use core::arch::asm;

use super::probe::{DEBUG_CONSOLE, Extensions, HART_STATE_MANAGEMENT, SYSTEM_RESET, SbiServices};

const BASE: usize = 0x10;
const HART_STATUS: usize = 2;
const STARTED: usize = 0;

pub(crate) fn extensions() -> Extensions {
    Extensions::probe(|function, extension| call(BASE, function, extension, 0, 0))
}

/// Probe the running SBI implementation for its HSM, timer and IPI services.
/// The current CPU must be executing in the supported SBI supervisor environment.
pub fn platform_services() -> SbiServices {
    SbiServices::probe(|function, extension| call(BASE, function, extension, 0, 0))
}

pub(crate) fn write_byte(byte: u8) -> bool {
    call(DEBUG_CONSOLE, 2, usize::from(byte), 0, 0).0 == 0
}

/// End the trusted boot path through SBI system reset.
pub(crate) fn reset(success: bool) {
    // Shutdown with no reason (0) or system failure (1).
    let _ = call(SYSTEM_RESET, 0, 0, usize::from(!success), 0);
}

pub(crate) fn start_hart(hart: usize, address: usize, opaque: usize) -> Result<(), isize> {
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

/// Check the live HSM state of the exact Hart described by the caller.
pub(crate) fn hart_started(hart: usize) -> bool {
    call(HART_STATE_MANAGEMENT, HART_STATUS, hart, 0, 0) == (0, STARTED)
}
