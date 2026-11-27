//! RISC-V supervisor time comparison. Access requires an actual Sstc grant.

#[cfg(all(limen_target, target_arch = "riscv64"))]
mod native;
#[cfg(all(limen_target, target_arch = "riscv64"))]
pub use native::Timer;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum State {
    Inherited(u64),
    Idle,
    Armed(u64),
    /// The required value and local enable could not both be confirmed.
    Programming(u64),
    /// Delivery is closed; a previously posted STIP may still be visible.
    Clearing,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Failure {
    Scope,
    Busy,
    Range,
    Unconfirmed,
}
