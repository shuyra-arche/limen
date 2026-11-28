//! Common operations on already admitted interrupt hardware.

pub(super) mod absolute;
pub(super) mod claim;
pub(super) mod ipi;
#[cfg(limen_target)]
pub(super) mod reminder;
pub(super) mod route;

pub use absolute::{AbsoluteTimer, TimerEffect};
pub use claim::{ClaimConfirmation, ClaimId, Completion, HardwareClaim, Receive};
pub use ipi::{Issuance, NotifyProcessor};
#[cfg(limen_target)]
pub use reminder::{Reminder, ReminderError, ReminderProgress};
pub use route::{RouteControl, RouteOperation, RouteState};

#[cfg(all(limen_target, target_arch = "riscv64"))]
pub use super::riscv64::internal::supervisor::Supervisor;
