//! Boot and firmware contracts.

#[cfg(any(limen_target, test))]
pub(crate) mod boot;
pub mod firmware;
