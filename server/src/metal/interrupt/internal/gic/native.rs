//! Privileged GIC capability inspection on the executing PE.
use core::arch::asm;

use super::GicV3;

impl GicV3<'_> {
    /// Inspect the executing PE's architectural GIC CPU interface feature.
    ///
    /// # Safety
    /// Run at an exception level allowed to read ID_AA64PFR0_EL1.
    pub unsafe fn cpu_interface_available() -> bool {
        let features: u64;
        unsafe { asm!("mrs {}, ID_AA64PFR0_EL1", out(reg) features, options(nomem, nostack)) };
        matches!((features >> 24) & 0xf, 1 | 3)
    }
}
