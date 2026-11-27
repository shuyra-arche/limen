//! Admitted SBI capabilities and the calls they authorize.

#[cfg(limen_firmware = "sbi")]
use super::super::sbi::{
    probe::{HART_STATE_MANAGEMENT, INTER_PROCESSOR_INTERRUPT, TIMER},
    transport::{BASE, call},
};

#[cfg(limen_firmware = "sbi")]
const HART_STATUS: usize = 2;
#[cfg(limen_firmware = "sbi")]
const STARTED: usize = 0;

/// A failed BASE call or invalid/unsupported version is not absence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProbeError {
    Firmware(isize),
    Version(usize),
}

/// An actual TIME call endpoint, admitted by the BASE probe. Exclusive
/// comparator authority remains with the consumer, not this service value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TimerService(pub(in crate::metal::platform::firmware) ());

/// An admitted HSM call endpoint, not authority over a particular target Hart.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HartService(pub(in crate::metal::platform::firmware) ());

/// A native SBI software interrupt service admitted by the BASE probe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IpiService(pub(in crate::metal::platform::firmware) ());

/// Services reported by the running SBI implementation's BASE extension.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SbiServices {
    pub(in crate::metal::platform::firmware) hart_state_management:
        Result<Option<HartService>, ProbeError>,
    pub(in crate::metal::platform::firmware) timer: Result<Option<TimerService>, ProbeError>,
    pub(in crate::metal::platform::firmware) ipi: Result<Option<IpiService>, ProbeError>,
}

impl SbiServices {
    pub fn hart_state_management(self) -> Result<Option<HartService>, ProbeError> {
        self.hart_state_management
    }

    pub fn timer(self) -> Result<Option<TimerService>, ProbeError> {
        self.timer
    }

    pub fn ipi(self) -> Result<Option<IpiService>, ProbeError> {
        self.ipi
    }
}

#[cfg(limen_firmware = "sbi")]
/// Probe the running SBI implementation for its HSM, timer and IPI services.
/// The current CPU must be executing in the supported SBI supervisor environment.
pub fn platform_services() -> Result<SbiServices, ProbeError> {
    SbiServices::probe(|function, extension| call(BASE, function, extension, 0, 0))
}

#[cfg(limen_firmware = "sbi")]
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

#[cfg(limen_firmware = "sbi")]
impl TimerService {
    /// The consumer owns the current Hart's comparator and its old obligations.
    pub(crate) fn set(self, count: u64) -> Result<(), isize> {
        match call(TIMER, 0, count as usize, 0, 0).0 {
            0 => Ok(()),
            error => Err(error),
        }
    }
}

#[cfg(limen_firmware = "sbi")]
impl IpiService {
    pub(crate) fn send(self, hart: usize) -> Result<(), isize> {
        match call(INTER_PROCESSOR_INTERRUPT, 0, 1, hart, 0).0 {
            0 => Ok(()),
            error => Err(error),
        }
    }
}
