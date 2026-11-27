//! Common operations on already admitted interrupt hardware.

pub(super) mod absolute;
pub(super) mod claim;
pub(super) mod ipi;
pub(super) mod route;

pub use absolute::AbsoluteTimer;
pub use claim::{Completion, HardwareClaim, Receive};
pub use ipi::{Issuance, NotifyHart};
pub use route::RouteControl;
