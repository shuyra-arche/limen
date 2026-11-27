//! One original PE's architected GIC CPU Interface.

/// Interpret ID_AA64PFR0_EL1.GIC for supported system register interfaces.
pub const fn system_register_interface(features: u64) -> bool {
    const SHIFT: u32 = 24;
    const GIC_V3_V4: u64 = 1;
    const GIC_V4_1: u64 = 3;
    matches!((features >> SHIFT) & 0xf, GIC_V3_V4 | GIC_V4_1)
}

#[cfg(all(limen_target, target_arch = "aarch64"))]
mod native;
#[cfg(all(limen_target, target_arch = "aarch64"))]
pub(super) use native::local;
#[cfg(all(limen_target, target_arch = "aarch64"))]
pub use native::{Claim, CpuInterface, Wake, available};
