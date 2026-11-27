//! Arm architected timebase.

#[cfg(all(limen_target, target_arch = "aarch64"))]
pub mod counter;
pub mod description;
