//! Firmware calls required by the trusted boot prefix.

use crate::metal::generic::services::Services;
use core::arch::asm;

mod probe;
use probe::Extensions;
use probe::{DEBUG_CONSOLE, SYSTEM_RESET};

const BASE: usize = 0x10;
const HART_STATE_MANAGEMENT: usize = 0x48534d;

fn extensions() -> Extensions {
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

pub(in crate::metal) fn start_hart(
    hart: usize,
    address: usize,
    opaque: usize,
) -> Result<(), isize> {
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

static SERVICES: Services = Services {
    output: Some(write_byte),
    reset: Some(reset),
};

/// The trap vector is live before BSS has been cleared. Its failure path
/// probes firmware directly and never depends on a published service table.
pub(crate) fn services() -> Services {
    let available = extensions();
    Services {
        output: SERVICES.output.filter(|_| available.console),
        reset: SERVICES.reset.filter(|_| available.reset),
    }
}

pub(crate) fn install() {
    let available = services();
    if available.output.is_none() || available.reset.is_none() {
        available.reject(format_args!(
            "FAIL Limen boot: required SBI debug console or system reset unavailable\n"
        ));
    }
    SERVICES.install();
}
