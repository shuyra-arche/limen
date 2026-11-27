//! RISC-V supervisor local reception lines, independent of their producers.

use core::marker::PhantomData;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Cause {
    Software,
    Timer,
    External,
}

impl Cause {
    fn mask(self) -> usize {
        match self {
            Self::Software => 1 << 1,
            Self::Timer => 1 << 5,
            Self::External => 1 << 9,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Context,
}

/// Native operations belonging to the original supervisor reception source.
struct Operations {
    valid: fn(u64) -> bool,
    enabled: fn() -> usize,
    set: fn(usize, bool),
}

/// Exclusive control of one supervisor reception line of the admitted Hart.
/// Does not install a Trap entry, grant global interrupt enable, or retire a
/// controller. The producer borrows this line for its whole active lifetime.
#[must_use]
pub struct Reception {
    hart: u64,
    cause: Cause,
    operations: &'static Operations,
    local: PhantomData<*mut ()>,
}

impl Reception {
    pub fn hart(&self) -> u64 {
        self.hart
    }

    pub fn cause(&self) -> Cause {
        self.cause
    }

    pub(crate) fn valid(&self) -> bool {
        (self.operations.valid)(self.hart)
    }

    pub(crate) fn enabled(&self) -> bool {
        (self.operations.enabled)() & self.cause.mask() != 0
    }

    pub(crate) fn set(&mut self, enabled: bool) {
        (self.operations.set)(self.cause.mask(), enabled);
    }
}

/// The two independently borrowable reception lines handed off by Entry.
/// Their ownership is not duplicated by controller admission.
#[must_use]
pub struct Supervisor {
    pub timer: Reception,
    pub external: Reception,
}

#[cfg(all(limen_target, target_arch = "riscv64"))]
mod native {
    use super::{Cause, Error, Operations, Reception, Supervisor};
    use crate::metal::processor::{executor::riscv64, treaty::try_current_hart};
    use core::{arch::asm, marker::PhantomData};

    fn valid(hart: u64) -> bool {
        try_current_hart().is_some_and(|current| current.get() as u64 == hart)
            && riscv64::native::status().masked()
    }

    pub(super) fn enabled() -> usize {
        let enabled: usize;
        unsafe { asm!("csrr {}, sie", out(reg) enabled, options(nostack, nomem)) };
        enabled
    }

    fn set(mask: usize, enabled: bool) {
        unsafe {
            if enabled {
                asm!("csrs sie, {}", in(reg) mask, options(nostack));
            } else {
                asm!("csrc sie, {}", in(reg) mask, options(nostack));
            }
        }
    }

    static OPERATIONS: Operations = Operations {
        valid,
        enabled,
        set,
    };

    impl Reception {
        fn new(hart: u64, cause: Cause) -> Self {
            Self {
                hart,
                cause,
                operations: &OPERATIONS,
                local: PhantomData,
            }
        }

        #[cfg(limen_firmware = "sbi")]
        pub(crate) unsafe fn admit_software(hart: u64) -> Result<Self, Error> {
            let software = Self::new(hart, Cause::Software);
            if !software.valid() || software.enabled() || software.pending() {
                return Err(Error::Context);
            }
            Ok(software)
        }

        #[cfg(limen_firmware = "sbi")]
        pub(crate) fn clear_software(&mut self) {
            unsafe { asm!("csrci sip, 2", options(nostack)) };
        }

        pub(crate) fn pending(&self) -> bool {
            let pending: usize;
            unsafe { asm!("csrr {}, sip", out(reg) pending, options(nostack, nomem)) };
            pending & self.cause.mask() != 0
        }
    }

    impl Supervisor {
        /// # Safety
        /// The trusted major entry owns this Hart's local lines and calls
        /// this exactly once, before delivering them to any consumer.
        pub(crate) unsafe fn admit(hart: u64) -> Result<Self, Error> {
            let timer = Reception::new(hart, Cause::Timer);
            let external = Reception::new(hart, Cause::External);
            if !timer.valid() || timer.enabled() || external.enabled() {
                return Err(Error::Context);
            }
            Ok(Self { timer, external })
        }
    }
}

/// Observe the actual full supervisor enable register; no control is granted.
#[cfg(all(limen_target, target_arch = "riscv64"))]
pub(in crate::metal) fn all_masked() -> bool {
    native::enabled() == 0
}
