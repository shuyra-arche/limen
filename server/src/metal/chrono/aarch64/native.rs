//! Privileged counter frequency inspection on the executing PE.
use core::{arch::asm, num::NonZeroU32};

use super::GenericCounter;
use crate::metal::platform::description::devicetree::Error;

impl<'tree> GenericCounter<'tree> {
    /// Admit the local virtual counter sourced by this descriptor.
    ///
    /// # Safety
    /// The caller establishes EL1 access to CNTVCT_EL0 and CNTFRQ_EL0 and
    /// retains the admitted virtual time domain while the clock is used.
    /// A changed virtual offset requires a new admission.
    pub unsafe fn clock(self, hart: usize) -> Result<crate::metal::chrono::Clock<'tree>, Error> {
        if !crate::metal::processor::hart::local::try_current_hart()
            .is_some_and(|current| current.get() == hart)
        {
            return Err(Error::Conflict);
        }
        let frequency = unsafe { self.counter_frequency() }?;
        crate::metal::chrono::Clock::bind(self.source, hart, frequency, read_virtual)
            .ok_or(Error::Conflict)
    }

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

fn read_virtual() -> u64 {
    let count;
    unsafe { asm!("mrs {}, CNTVCT_EL0", out(reg) count, options(nomem, nostack)) };
    count
}
