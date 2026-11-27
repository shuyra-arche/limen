//! Actual control-flow transfer and machine execution descriptions.

#[cfg(limen_cooperative)]
pub(super) mod cooperative;

pub(in crate::metal) mod terminal;

mod profile;
