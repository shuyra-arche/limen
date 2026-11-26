//! Native PSCI calls through the verified conduit.
use core::arch::asm;

use super::{Conduit, PsciDescription, PsciError, PsciVersion};

const VERSION: usize = 0x8400_0000;
const CPU_ON: usize = 0xc400_0003;
const AFFINITY_INFO: usize = 0xc400_0004;
const SYSTEM_OFF: usize = 0x8400_0008;
const FEATURES: usize = 0x8400_000a;

impl PsciDescription<'_> {
    /// Query the live PSCI version and the calls required by this boot path.
    ///
    /// # Safety
    /// The caller must establish that this conduit reaches PSCI from the
    /// current exception level. A tree from an unrelated platform is invalid.
    pub unsafe fn verify(self) -> Result<PsciVersion, PsciError> {
        let raw = unsafe { self.conduit.invoke(VERSION, 0, 0, 0) } as i32;
        if raw < 0 {
            return Err(PsciError::Firmware(raw));
        }
        let version = PsciVersion {
            major: (raw >> 16) as u16,
            minor: (raw & 0xffff) as u16,
        };
        let minimum = self.minimum_version();
        if (version.major, version.minor) < (minimum.major, minimum.minor) {
            return Err(PsciError::Version);
        }
        if version.major >= 1 {
            for function in [CPU_ON, AFFINITY_INFO, SYSTEM_OFF] {
                let raw = unsafe { self.conduit.invoke(FEATURES, function, 0, 0) } as i32;
                if raw < 0 {
                    return Err(PsciError::Firmware(raw));
                }
            }
        }
        Ok(version)
    }

    /// Check the current PSCI state of a described affinity at level zero.
    ///
    /// # Safety
    /// The caller must have verified the conduit and the target affinity's scope.
    pub unsafe fn verify_affinity(self, affinity: usize, online: bool) -> Result<(), PsciError> {
        let raw = unsafe { self.conduit.invoke(AFFINITY_INFO, affinity, 0, 0) } as i32;
        match raw {
            0 if online => Ok(()),
            1 if !online => Ok(()),
            code if code < 0 => Err(PsciError::Firmware(code)),
            _ => Err(PsciError::Affinity),
        }
    }
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
        match unsafe { self.invoke(CPU_ON, hart, entry.addr(), context) as i32 } {
            0 => Ok(()),
            code => Err(code as isize),
        }
    }

    /// Request SYSTEM_OFF through this conduit.
    ///
    /// # Safety
    /// The caller must have verified the conduit and own platform shutdown.
    pub unsafe fn system_off(self) {
        let _ = unsafe { self.invoke(SYSTEM_OFF, 0, 0, 0) };
    }
}
