//! Actual local and external interrupt hardware mechanisms.

pub mod aarch64;
pub mod riscv64;
pub mod x86_64;

mod claim;
pub use claim::{Completion, HardwareClaim};

mod absolute;
pub use absolute::AbsoluteTimer;

mod publication;
pub use publication::PublishRoute;
