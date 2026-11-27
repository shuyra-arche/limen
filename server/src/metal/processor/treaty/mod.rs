//! Physical identity and checked local machine access for Processor consumers.

#[cfg(any(limen_target, test))]
use super::executor::local;
#[cfg(limen_cooperative)]
pub use super::executor::local::Major;
pub(in crate::metal) use super::executor::local::Protection;

#[cfg(limen_cooperative)]
mod cooperative;
#[cfg(limen_cooperative)]
pub use cooperative::Dispatchers;
#[cfg(limen_cooperative)]
pub(crate) use cooperative::{
    Context, Dispatcher, LeaveReceipt, LocalExecution, attached, cooperative_ready, current,
    reserve,
};

/// Physical Hart identity used to compare observations in their original scope.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HartId(usize);

impl HartId {
    pub const fn new(value: usize) -> Self {
        Self(value)
    }

    pub const fn get(self) -> usize {
        self.0
    }
}

/// Check the ordinary entry and return only its installed Hart identity.
#[cfg(limen_target)]
pub(crate) fn try_current_hart() -> Option<HartId> {
    Some(HartId::new(local::ordinary_local()?.identity()))
}

/// Protect a window of execution on the current verified CPU.
#[cfg(any(limen_target, test))]
pub(in crate::metal) fn protect() -> Option<Protection> {
    let local = local::ordinary_local()?;
    Some(unsafe { local.acquire_protection() })
}

#[cfg(not(any(limen_target, test)))]
pub(in crate::metal) fn protect() -> Option<Protection> {
    None
}

/// Observe whether the current ordinary CPU has no outstanding protection.
#[cfg(any(limen_target, test))]
pub(crate) fn can_switch() -> bool {
    protect().is_some_and(|protection| protection.is_outer())
}

pub use super::execution::terminal::Console;
#[cfg(limen_target)]
pub use super::execution::terminal::Terminal;
#[cfg(limen_target)]
pub use super::executor::launch::{Minor, StartError, Startup};
