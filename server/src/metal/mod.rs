#[cfg(any(limen_target, test))]
pub(crate) mod identity;
pub mod interrupt;
pub mod memory;
pub mod module;
pub mod platform;
pub mod processor;
#[cfg(any(limen_target, test))]
pub mod sync;
pub mod time;
