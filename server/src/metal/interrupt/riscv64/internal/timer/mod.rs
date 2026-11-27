//! One supervisor time comparator, with native Sstc or firmware SBI TIME programming.

#[cfg(all(limen_target, target_arch = "riscv64"))]
mod native;
#[cfg(all(limen_target, target_arch = "riscv64"))]
pub use native::Timer;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Failure {
    Scope,
    Context,
    Busy,
    Range,
    Firmware(isize),
    Unconfirmed,
}

/// The comparison write has not been confirmed, or the comparison is
/// confirmed but the local timer enable is still owed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProgramStep {
    Compare,
    Enable,
}

/// One synchronous replacement of the actual comparator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Programming {
    pub previous: Option<u64>,
    pub requested: u64,
    pub step: ProgramStep,
}

/// The original comparison facts that a clearing operation must retain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Prior {
    Inherited(Option<u64>),
    Idle,
    Armed(u64),
    Programming(Programming),
}

/// The stage of the original clear whose effect is still owed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClearStep {
    Mask,
    Compare,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Clearing {
    pub prior: Prior,
    pub step: ClearStep,
}

/// The specific operation that returned without confirmation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Effect {
    Programming(Programming),
    Clearing(Clearing),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum State {
    Inherited(Option<u64>),
    Idle,
    Programming(Programming),
    Armed(u64),
    Clearing(Clearing),
    AwaitingClear(Prior),
    Uncertain(Effect),
}

/// An admission request or an observation from the current hardware step.
/// Values describe the detached state; the Timer exclusively holds the live state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Event {
    Arm { now: u64, target: u64 },
    Rearm { now: u64, target: u64 },
    Shut,
    Compared(Result<(), Failure>),
    Enabled(bool),
    Masked(bool),
    Drained(bool),
}

impl State {
    /// The last confirmed active comparison for this owner. Inherited
    /// values and effects awaiting recovery cannot authorize expiry.
    pub const fn deadline(self) -> Option<u64> {
        match self {
            Self::Armed(count) => Some(count),
            _ => None,
        }
    }

    /// Advance the comparator record using the original step's observation.
    /// Counter values in an admission request belong to the same Chrono domain.
    pub fn advance(&mut self, event: Event) -> Result<(), Failure> {
        let (next, result) = match (*self, event) {
            (Self::Idle, Event::Arm { now, target }) => (Self::program(None, now, target)?, Ok(())),
            (Self::Armed(old), Event::Rearm { now, target }) => {
                (Self::program(Some(old), now, target)?, Ok(()))
            }
            (_, Event::Shut) => (self.clearing()?, Ok(())),
            (Self::Programming(program), Event::Compared(result))
                if program.step == ProgramStep::Compare =>
            {
                let next = if result.is_ok() {
                    Self::Programming(Programming {
                        step: ProgramStep::Enable,
                        ..program
                    })
                } else {
                    Self::Uncertain(Effect::Programming(program))
                };
                (next, result)
            }
            (Self::Programming(program), Event::Enabled(confirmed))
                if program.step == ProgramStep::Enable =>
            {
                if confirmed {
                    (Self::Armed(program.requested), Ok(()))
                } else {
                    (
                        Self::Uncertain(Effect::Programming(program)),
                        Err(Failure::Unconfirmed),
                    )
                }
            }
            (Self::Clearing(clear), Event::Masked(confirmed)) if clear.step == ClearStep::Mask => {
                if confirmed {
                    (
                        Self::Clearing(Clearing {
                            step: ClearStep::Compare,
                            ..clear
                        }),
                        Ok(()),
                    )
                } else {
                    (
                        Self::Uncertain(Effect::Clearing(clear)),
                        Err(Failure::Unconfirmed),
                    )
                }
            }
            (Self::Clearing(clear), Event::Compared(result))
                if clear.step == ClearStep::Compare =>
            {
                let next = if result.is_ok() {
                    Self::AwaitingClear(clear.prior)
                } else {
                    Self::Uncertain(Effect::Clearing(clear))
                };
                (next, result)
            }
            (Self::AwaitingClear(_), Event::Drained(true)) => (Self::Idle, Ok(())),
            (Self::AwaitingClear(_), Event::Drained(false)) => return Err(Failure::Busy),
            _ => return Err(Failure::Busy),
        };
        *self = next;
        result
    }

    fn program(previous: Option<u64>, now: u64, target: u64) -> Result<Self, Failure> {
        if target == u64::MAX || target <= now {
            return Err(Failure::Range);
        }
        Ok(Self::Programming(Programming {
            previous,
            requested: target,
            step: ProgramStep::Compare,
        }))
    }

    fn clearing(self) -> Result<Self, Failure> {
        let prior = match self {
            Self::Inherited(count) => Prior::Inherited(count),
            Self::Idle => Prior::Idle,
            Self::Armed(count) => Prior::Armed(count),
            Self::Uncertain(Effect::Programming(program)) => Prior::Programming(program),
            Self::Uncertain(Effect::Clearing(clear)) => clear.prior,
            Self::AwaitingClear(_) => return Ok(self),
            Self::Programming(_) | Self::Clearing(_) => return Err(Failure::Busy),
        };
        Ok(Self::Clearing(Clearing {
            prior,
            step: ClearStep::Mask,
        }))
    }
}

#[cfg(test)]
#[path = "../../../../../../tests/metal/interrupt/riscv64/internal/timer/mod.rs"]
mod tests;
