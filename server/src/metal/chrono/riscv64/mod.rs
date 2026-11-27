//! RISC-V timebase.

pub mod description;
#[cfg(all(limen_target, target_arch = "riscv64"))]
pub mod time;
