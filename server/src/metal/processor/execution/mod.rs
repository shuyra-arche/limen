//! Actual control-flow transfer and machine execution descriptions.

#[cfg(limen_cooperative)]
pub(super) mod cooperative;

#[cfg(limen_target)]
pub(super) mod runtime;

pub(crate) mod terminal;

mod profile;
