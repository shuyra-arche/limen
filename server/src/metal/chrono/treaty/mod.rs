//! Use the original admitted counter through Chrono.

mod clock;
pub use clock::{Clock, Deadline, Sample};

#[cfg(all(limen_cooperative, target_arch = "riscv64"))]
mod meter;
#[cfg(all(limen_cooperative, target_arch = "riscv64"))]
pub(crate) use meter::{Meter, MeterReceipt};
#[cfg(any(limen_target, test))]
mod timing;
#[cfg(limen_target)]
pub use timing::{DurationBounds, LocalTiming, StopBounds, TimingBounds};
