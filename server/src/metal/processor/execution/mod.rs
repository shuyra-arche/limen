//! Actual control-flow transfer and machine execution descriptions.

#[cfg(limen_cooperative)]
pub(super) mod cooperative;

pub(crate) mod terminal;

mod profile;
