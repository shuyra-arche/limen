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
#[cfg(all(limen_cooperative, target_arch = "riscv64"))]
pub(crate) use cooperative::{budget_ready, current_in_interrupt};

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

/// The original local entry grants at most one supervisor line delivery.
#[cfg(all(limen_target, target_arch = "riscv64"))]
pub(in crate::metal) fn claim_supervisor_reception(processor: ProcessorId) -> bool {
    local::ordinary_local()
        .is_some_and(|local| local.identity() == processor && local.claim_supervisor_reception())
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

#[cfg(all(limen_target, any(target_arch = "aarch64", target_arch = "x86_64")))]
pub use super::execution::runtime::Irq;
#[cfg(limen_target)]
pub(crate) use super::execution::runtime::{Capture, Handler};
#[cfg(limen_target)]
pub use super::execution::runtime::{Entry, InstallError, RuntimeTrap};
#[cfg(all(limen_target, target_arch = "riscv64"))]
pub use super::execution::runtime::{SoftwareInterrupt, TimerInterrupt};

#[cfg(all(limen_target, target_arch = "aarch64"))]
pub(in crate::metal) use super::executor::aarch64::native::wait_for_interrupt;
#[cfg(all(limen_target, target_arch = "riscv64", limen_firmware = "sbi"))]
pub(in crate::metal) use super::executor::riscv64::native::wait_for_interrupt;
#[cfg(all(limen_target, target_arch = "x86_64"))]
pub(in crate::metal) use super::executor::x86_64::native::wait_for_interrupt;

pub use super::execution::terminal::Console;
#[cfg(limen_target)]
pub use super::execution::terminal::Terminal;
#[cfg(limen_target)]
pub use super::executor::launch::{Minor, StartError, Startup};

#[cfg(limen_target)]
pub use super::executor::local::{LocalStorage, stack::Stack};
