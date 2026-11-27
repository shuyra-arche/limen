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
    Context, Dispatcher, LeaveReceipt, LocalExecution, RuntimeError, cooperative_ready, current,
    start_runtime,
};

/// Hardware identity of a logical processor in its original scope.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcessorId(usize);

impl ProcessorId {
    pub const fn new(value: usize) -> Self {
        Self(value)
    }

    pub const fn get(self) -> usize {
        self.0
    }
}

/// Check the ordinary entry and return only its installed processor identity.
#[cfg(limen_target)]
pub(crate) fn try_current_processor() -> Option<ProcessorId> {
    Some(local::ordinary_local()?.identity())
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

#[cfg(limen_target)]
pub use super::executor::local::{LocalStorage, stack::Stack};

#[cfg(limen_firmware = "sbi")]
pub use super::description::riscv64::cpu::RiscvCpus;
