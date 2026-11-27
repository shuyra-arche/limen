pub(super) const DEBUG_CONSOLE: usize = 0x4442_434e;
pub(super) const SYSTEM_RESET: usize = 0x5352_5354;
pub(super) const HART_STATE_MANAGEMENT: usize = 0x0048_534d;
pub(super) const TIMER: usize = 0x5449_4d45;
pub(super) const INTER_PROCESSOR_INTERRUPT: usize = 0x0073_5049;

/// A failed BASE call or invalid/unsupported version is not absence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProbeError {
    Firmware(isize),
    Version(usize),
}

pub(crate) struct Extensions {
    pub(crate) console: bool,
    pub(crate) reset: bool,
}

impl Extensions {
    pub(crate) fn probe(mut base: impl FnMut(usize, usize) -> (isize, usize)) -> Self {
        // Terminal diagnostics may only select positively confirmed services.
        let modern = modern(&mut base).is_ok();
        Self {
            console: modern && supports(&mut base, DEBUG_CONSOLE) == Ok(true),
            reset: modern && supports(&mut base, SYSTEM_RESET) == Ok(true),
        }
    }
}

/// An actual TIME call endpoint, admitted by the BASE probe. Exclusive
/// comparator authority remains with the consumer, not this service value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TimerService(());

/// An admitted HSM call endpoint, not authority over a particular target Hart.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HartService(());

/// A native SBI software interrupt service admitted by the BASE probe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IpiService(());

/// Services reported by the running SBI implementation's BASE extension.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SbiServices {
    hart_state_management: Result<Option<HartService>, ProbeError>,
    timer: Result<Option<TimerService>, ProbeError>,
    ipi: Result<Option<IpiService>, ProbeError>,
}

impl SbiServices {
    pub(crate) fn probe(
        mut base: impl FnMut(usize, usize) -> (isize, usize),
    ) -> Result<Self, ProbeError> {
        modern(&mut base)?;
        Ok(Self {
            hart_state_management: supports(&mut base, HART_STATE_MANAGEMENT)
                .map(|present| present.then_some(HartService(()))),
            timer: supports(&mut base, TIMER).map(|present| present.then_some(TimerService(()))),
            ipi: supports(&mut base, INTER_PROCESSOR_INTERRUPT)
                .map(|present| present.then_some(IpiService(()))),
        })
    }

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

fn modern(base: &mut impl FnMut(usize, usize) -> (isize, usize)) -> Result<(), ProbeError> {
    let (error, version) = base(0, 0);
    if error != 0 {
        return Err(ProbeError::Firmware(error));
    }
    if !(2..(1 << 31)).contains(&version) {
        return Err(ProbeError::Version(version));
    }
    Ok(())
}

fn supports(
    base: &mut impl FnMut(usize, usize) -> (isize, usize),
    extension: usize,
) -> Result<bool, ProbeError> {
    let (error, value) = base(3, extension);
    if error != 0 {
        return Err(ProbeError::Firmware(error));
    }
    Ok(value != 0)
}

#[cfg(test)]
#[path = "../../../../../tests/metal/platform/firmware/sbi/mod.rs"]
mod tests;
