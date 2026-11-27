//! Actual native x86 local-control observation, without MMIO ownership.

use core::arch::{asm, x86_64::__cpuid};

use crate::metal::{
    platform::description::acpi::Madt,
    processor::{description::x86_64::status, executor::x86_64::native, treaty::try_current_hart},
};

use super::{Base, Observation, Probe};

const APIC_BASE: u32 = 0x1b;
const FEATURE_MSR: u32 = 1 << 5;
const FEATURE_APIC: u32 = 1 << 9;

fn base_bits() -> Option<u64> {
    let cs: u16;
    unsafe { asm!("mov {:x}, cs", out(reg) cs, options(nomem, nostack, preserves_flags)) };
    if !status::kernel_selector(cs) {
        return None;
    }
    let features = __cpuid(1);
    if features.edx & (FEATURE_MSR | FEATURE_APIC) != (FEATURE_MSR | FEATURE_APIC) {
        return None;
    }
    let (low, high): (u32, u32);
    unsafe {
        asm!("rdmsr", in("ecx") APIC_BASE, out("eax") low, out("edx") high,
            options(nomem, nostack, preserves_flags));
    }
    Some((u64::from(high) << 32) | u64::from(low))
}

fn masked() -> bool {
    let flags: usize;
    unsafe { asm!("pushfq", "pop {}", out(reg) flags, options(preserves_flags)) };
    status::interrupts_masked(flags)
}

/// Early discovery precedes installation of the ordinary Hart-local record.
pub(crate) fn matches_madt(hart: usize, madt: Madt<'_>) -> bool {
    base_bits().and_then(Base::decode).is_some_and(|base| {
        base.major && native::boot_identity() == hart && base.matches(hart, madt)
    })
}

fn observe() -> Option<Observation> {
    let bits = base_bits()?;
    let hart = native::boot_identity();
    if !masked() || try_current_hart().is_none_or(|current| current.get() != hart) {
        return None;
    }
    Some(Observation { bits, hart })
}

impl Probe {
    /// Bind the actual native observation source. No hardware capability is
    /// asserted until the source is read in the current ordinary environment.
    pub const fn native() -> Self {
        Self { read: observe }
    }
}
