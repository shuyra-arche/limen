//! Privileged counter frequency inspection on the executing PE.
use core::{arch::asm, num::NonZeroU32};

use super::description::GenericCounter;
use crate::metal::{
    chrono::treaty::Clock,
    platform::description::devicetree::{Error, NodeId},
    processor::treaty::try_current_hart,
};

/// Original virtual counter and its firmware scope for the native comparator.
#[must_use]
pub struct VirtualCounter<'tree> {
    source: NodeId<'tree>,
    clock: Clock,
}

impl<'tree> VirtualCounter<'tree> {
    pub fn source(&self) -> NodeId<'tree> {
        self.source
    }

    pub fn clock(&self) -> &Clock {
        &self.clock
    }
}

impl<'tree> GenericCounter<'tree> {
    /// Admit the local virtual counter sourced by this descriptor.
    ///
    /// # Safety
    /// The caller establishes EL1 access to CNTVCT_EL0 and CNTFRQ_EL0 and
    /// retains the admitted virtual time domain while the clock is used.
    /// A changed virtual offset requires a new admission.
    pub unsafe fn bind(self, hart: usize) -> Result<VirtualCounter<'tree>, Error> {
        let hart = try_current_hart()
            .filter(|current| current.get() == hart)
            .ok_or(Error::Conflict)?;
        let frequency = unsafe { self.counter_frequency() }?;
        let clock = Clock::bind(hart, frequency.into(), read_virtual).ok_or(Error::Conflict)?;
        Ok(VirtualCounter {
            source: self.source,
            clock,
        })
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
