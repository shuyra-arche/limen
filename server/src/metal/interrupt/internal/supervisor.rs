//! RISC-V supervisor local reception lines, independent of their producers.

use crate::metal::processor::hart::local::try_current_hart;
use core::{arch::asm, marker::PhantomData};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Cause {
    Timer,
    External,
}

impl Cause {
    fn mask(self) -> usize {
        match self {
            Self::Timer => 1 << 5,
            Self::External => 1 << 9,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Context,
}

/// Exclusive control of one supervisor reception line of the admitted Hart.
/// Does not install a Trap entry, grant global interrupt enable, or retire a
/// controller. The producer borrows this line for its whole active lifetime.
#[must_use]
pub struct Reception {
    hart: u64,
    cause: Cause,
    local: PhantomData<*mut ()>,
}

impl Reception {
    fn new(hart: u64, cause: Cause) -> Self {
        Self {
            hart,
            cause,
            local: PhantomData,
        }
    }

    pub fn hart(&self) -> u64 {
        self.hart
    }

    pub fn cause(&self) -> Cause {
        self.cause
    }

    pub(crate) fn valid(&self) -> bool {
        let status: usize;
        unsafe { asm!("csrr {}, sstatus", out(reg) status, options(nostack, nomem)) };
        try_current_hart().is_some_and(|hart| hart.get() as u64 == self.hart) && status & 2 == 0
    }

    pub(crate) fn enabled(&self) -> bool {
        let enabled: usize;
        unsafe { asm!("csrr {}, sie", out(reg) enabled, options(nostack, nomem)) };
        enabled & self.cause.mask() != 0
    }

    pub(crate) fn pending(&self) -> bool {
        let pending: usize;
        unsafe { asm!("csrr {}, sip", out(reg) pending, options(nostack, nomem)) };
        pending & self.cause.mask() != 0
    }

    pub(crate) fn set(&mut self, enabled: bool) {
        let mask = self.cause.mask();
        unsafe {
            if enabled {
                asm!("csrs sie, {}", in(reg) mask, options(nostack));
            } else {
                asm!("csrc sie, {}", in(reg) mask, options(nostack));
            }
        }
    }
}

/// The two independently borrowable reception lines handed off by Entry.
/// Their ownership is not duplicated by controller admission.
#[must_use]
pub struct Supervisor {
    pub timer: Reception,
    pub external: Reception,
}

impl Supervisor {
    /// # Safety
    /// The trusted major entry owns this Hart's local lines and calls this
    /// exactly once, before delivering them to any consumer.
    pub(crate) unsafe fn admit(hart: u64) -> Result<Self, Error> {
        let timer = Reception::new(hart, Cause::Timer);
        let external = Reception::new(hart, Cause::External);
        if !timer.valid() || timer.enabled() || external.enabled() {
            return Err(Error::Context);
        }
        Ok(Self { timer, external })
    }
}
