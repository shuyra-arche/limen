//! Privileged counter frequency inspection on the executing PE.
use core::{arch::asm, num::NonZeroU32};

use super::GenericTimer;
use crate::metal::platform::firmware::devicetree::Error;

impl GenericTimer<'_> {
    /// Read the frequency from the current PE and compare an explicit DT value.
    ///
    /// # Safety
    /// Run at an exception level allowed to read CNTFRQ_EL0.
    pub unsafe fn counter_frequency(self) -> Result<NonZeroU32, Error> {
        let frequency: u64;
        unsafe { asm!("mrs {}, CNTFRQ_EL0", out(reg) frequency, options(nomem, nostack)) };
        let frequency = u32::try_from(frequency)
            .ok()
            .and_then(NonZeroU32::new)
            .ok_or(Error::Range)?;
        if self
            .frequency
            .is_some_and(|described| described != frequency)
        {
            return Err(Error::Conflict);
        }
        Ok(frequency)
    }
}
