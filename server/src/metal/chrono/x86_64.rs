//! Local TSC admission from architectural frequency information.

use core::num::NonZeroU64;

fn frequency(denominator: u32, numerator: u32, crystal_hertz: u32) -> Option<NonZeroU64> {
    let denominator = NonZeroU64::new(u64::from(denominator))?;
    let scaled = u64::from(numerator) * u64::from(crystal_hertz);
    // The common Clock represents whole Hertz. Reject a fractional result.
    if !scaled.is_multiple_of(denominator.get()) {
        return None;
    }
    NonZeroU64::new(scaled / denominator)
}

#[cfg(all(limen_target, target_arch = "x86_64"))]
pub use native::{Failure, Tsc};

#[cfg(all(limen_target, target_arch = "x86_64"))]
mod native {
    use core::arch::{asm, x86_64::__cpuid};

    use crate::metal::{chrono::Clock, processor::hart::local::try_current_hart};

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum Failure {
        Context,
        Absent,
        VariableRate,
        Frequency,
    }

    /// The original invariant TSC on the bound local Hart.
    #[must_use]
    pub struct Tsc {
        clock: Clock,
    }

    impl Tsc {
        /// Admit a TSC whose nominal rate is established by CPUID.15H.
        ///
        /// # Safety
        /// The current execution privilege must permit TSC access. The
        /// platform must preserve this Hart's TSC domain and its enumerated
        /// rate while the clock is live. TSC writes, offset or scale changes,
        /// and migration to a different time domain require a new admission.
        pub unsafe fn admit() -> Result<Self, Failure> {
            let hart = try_current_hart().ok_or(Failure::Context)?;
            let basic = __cpuid(0).eax;
            if basic < 1 || __cpuid(1).edx & (1 << 4) == 0 {
                return Err(Failure::Absent);
            }
            if __cpuid(0x8000_0000).eax < 0x8000_0007 || __cpuid(0x8000_0007).edx & (1 << 8) == 0 {
                return Err(Failure::VariableRate);
            }
            if basic < 0x15 {
                return Err(Failure::Frequency);
            }
            let rate = __cpuid(0x15);
            let hertz = super::frequency(rate.eax, rate.ebx, rate.ecx).ok_or(Failure::Frequency)?;
            let clock = Clock::bind(hart, hertz, read).ok_or(Failure::Context)?;
            Ok(Self { clock })
        }

        pub fn clock(&self) -> &Clock {
            &self.clock
        }
    }

    fn read() -> u64 {
        let count;
        // CPUID serializes the local observation and preserves ordering
        // without requiring a particular vendor's LFENCE behavior.
        unsafe {
            asm!(
                "mov r8, rbx",
                "xor eax, eax",
                "cpuid",
                "rdtsc",
                "shl rdx, 32",
                "or rax, rdx",
                "mov r9, rax",
                "xor eax, eax",
                "cpuid",
                "mov rbx, r8",
                out("eax") _,
                out("ecx") _,
                out("edx") _,
                out("r8") _,
                out("r9") count,
                options(nostack),
            )
        };
        count
    }
}

#[cfg(test)]
#[path = "../../../tests/metal/chrono/x86_64.rs"]
mod tests;
