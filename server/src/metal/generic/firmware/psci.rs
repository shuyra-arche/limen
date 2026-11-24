//! PSCI calls through the conduit advertised by firmware.
use core::arch::asm;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Conduit {
    Hvc,
    Smc,
}

impl Conduit {
    unsafe fn invoke(self, function: usize, first: usize, second: usize, third: usize) -> isize {
        let result: isize;
        macro_rules! invoke {
            ($insn:literal) => {
                unsafe { asm!("dsb sy", $insn, inout("x0") function => result,
                    in("x1") first, in("x2") second, in("x3") third,
                    clobber_abi("C"), options(nostack)) }
            };
        }
        match self {
            Self::Hvc => invoke!("hvc #0"),
            Self::Smc => invoke!("smc #0"),
        }
        result
    }

    pub(in crate::metal) unsafe fn cpu_on(
        self,
        hart: usize,
        entry: *const (),
        context: usize,
    ) -> Result<(), isize> {
        match unsafe { self.invoke(0xc4000003, hart, entry.addr(), context) } {
            0 => Ok(()),
            code => Err(code),
        }
    }

    /// Request SYSTEM_OFF through this conduit.
    ///
    /// # Safety
    /// The caller must have verified the conduit and own platform shutdown.
    pub unsafe fn system_off(self) {
        let _ = unsafe { self.invoke(0x84000008, 0, 0, 0) };
    }
}
