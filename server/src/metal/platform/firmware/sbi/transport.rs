//! Firmware calls required by the trusted boot prefix.

use core::arch::asm;

use super::probe::{
    DEBUG_CONSOLE, Extensions, HART_STATE_MANAGEMENT, HartService, INTER_PROCESSOR_INTERRUPT,
    IpiService, ProbeError, SYSTEM_RESET, SbiServices, TIMER, TimerService,
};

const BASE: usize = 0x10;
const HART_STATUS: usize = 2;
const STARTED: usize = 0;

pub(crate) fn extensions() -> Extensions {
    Extensions::probe(|function, extension| call(BASE, function, extension, 0, 0))
}

/// Probe the running SBI implementation for its HSM, timer and IPI services.
/// The current CPU must be executing in the supported SBI supervisor environment.
pub fn platform_services() -> Result<SbiServices, ProbeError> {
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

impl HartService {
    /// Check the live state of the exact Hart described by the caller.
    pub(crate) fn started(self, hart: usize) -> bool {
        call(HART_STATE_MANAGEMENT, HART_STATUS, hart, 0, 0) == (0, STARTED)
    }

    pub(crate) fn start(self, hart: usize, address: usize, opaque: usize) -> Result<(), isize> {
        match call(HART_STATE_MANAGEMENT, 0, hart, address, opaque).0 {
            0 => Ok(()),
            error => Err(error),
        }
    }
}

impl TimerService {
    /// The consumer owns the current Hart's comparator and its old obligations.
    pub(crate) fn set(self, count: u64) -> Result<(), isize> {
        match call(TIMER, 0, count as usize, 0, 0).0 {
            0 => Ok(()),
            error => Err(error),
        }
    }
}

impl IpiService {
    pub(crate) fn send(self, hart: usize) -> Result<(), isize> {
        match call(INTER_PROCESSOR_INTERRUPT, 0, 1, hart, 0).0 {
            0 => Ok(()),
            error => Err(error),
        }
    }
}
