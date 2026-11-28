//! Native PSCI calls through the verified conduit.
use core::arch::asm;

use super::{Conduit, Psci};

pub(in crate::metal::platform::firmware) const VERSION: usize = 0x8400_0000;
pub(in crate::metal::platform::firmware) const CPU_ON: usize = 0xc400_0003;
pub(in crate::metal::platform::firmware) const AFFINITY_INFO: usize = 0xc400_0004;
pub(in crate::metal::platform::firmware) const SYSTEM_OFF: usize = 0x8400_0008;
pub(in crate::metal::platform::firmware) const FEATURES: usize = 0x8400_000a;

impl Psci {
    pub(in crate::metal) unsafe fn cpu_on(
        self,
        processor: usize,
        entry: *const (),
        context: usize,
    ) -> Result<(), isize> {
        match unsafe {
            self.conduit
                .invoke(CPU_ON, processor, entry.addr(), context) as i32
        } {
            0 => Ok(()),
            code => Err(code as isize),
        }
    }
}

impl Conduit {
    pub(in crate::metal::platform::firmware) unsafe fn invoke(
        self,
        function: usize,
        first: usize,
        second: usize,
        third: usize,
    ) -> isize {
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
}
