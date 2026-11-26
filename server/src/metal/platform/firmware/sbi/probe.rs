pub(super) const DEBUG_CONSOLE: usize = 0x4442_434e;
pub(super) const SYSTEM_RESET: usize = 0x5352_5354;
pub(super) const HART_STATE_MANAGEMENT: usize = 0x0048_534d;
pub(super) const TIMER: usize = 0x5449_4d45;
const INTER_PROCESSOR_INTERRUPT: usize = 0x0073_5049;

pub(crate) struct Extensions {
    pub(crate) console: bool,
    pub(crate) reset: bool,
}

impl Extensions {
    pub(crate) fn probe(mut base: impl FnMut(usize, usize) -> (isize, usize)) -> Self {
        let modern = modern(&mut base);
        Self {
            console: modern && supports(&mut base, DEBUG_CONSOLE),
            reset: modern && supports(&mut base, SYSTEM_RESET),
        }
    }
}

/// Availability reported by the running SBI implementation's BASE extension.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SbiServices {
    hart_state_management: bool,
    timer: bool,
    inter_processor_interrupt: bool,
}

impl SbiServices {
    pub(crate) fn probe(mut base: impl FnMut(usize, usize) -> (isize, usize)) -> Self {
        let modern = modern(&mut base);
        Self {
            hart_state_management: modern && supports(&mut base, HART_STATE_MANAGEMENT),
            timer: modern && supports(&mut base, TIMER),
            inter_processor_interrupt: modern && supports(&mut base, INTER_PROCESSOR_INTERRUPT),
        }
    }

    pub fn hart_state_management(self) -> bool {
        self.hart_state_management
    }

    pub fn timer(self) -> bool {
        self.timer
    }

    pub fn inter_processor_interrupt(self) -> bool {
        self.inter_processor_interrupt
    }
}

fn modern(base: &mut impl FnMut(usize, usize) -> (isize, usize)) -> bool {
    let (error, version) = base(0, 0);
    error == 0 && (2..(1 << 31)).contains(&version)
}

fn supports(base: &mut impl FnMut(usize, usize) -> (isize, usize), extension: usize) -> bool {
    let (error, value) = base(3, extension);
    error == 0 && value != 0
}

#[cfg(test)]
#[path = "../../../../../tests/metal/platform/firmware/sbi/mod.rs"]
mod tests;
