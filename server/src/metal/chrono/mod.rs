mod clock;
pub use clock::{Clock, Deadline, Sample};

#[cfg(limen_target)]
mod native;
#[cfg(limen_target)]
pub use native::LocalClock;

pub mod aarch64;
pub mod riscv64;
pub mod x86_64;
