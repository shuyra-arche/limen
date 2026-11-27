mod clock;
pub use clock::{Clock, Deadline, Sample};

pub mod aarch64;
pub mod riscv64;
#[cfg(any(test, all(limen_target, target_arch = "x86_64")))]
pub mod x86_64;
