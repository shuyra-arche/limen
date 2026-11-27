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

impl State {
    /// The last confirmed active comparison for this owner. Inherited
    /// values and effects awaiting recovery cannot authorize expiry.
    pub const fn deadline(self) -> Option<u64> {
        match self {
            Self::Armed(count) => Some(count),
            _ => None,
        }
    }
}

#[cfg(test)]
#[path = "../../../../../../tests/metal/interrupt/riscv64/internal/timer/mod.rs"]
mod tests;
