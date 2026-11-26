use core::arch::{asm, x86_64::__cpuid};

pub(crate) fn boot_identity() -> usize {
    (__cpuid(1).ebx >> 24) as usize
}

/// Compare the admitted boot CPU and its active local controller to MADT.
pub(in crate::metal) fn matches_madt(
    hart: usize,
    madt: crate::metal::firmware::acpi::Madt<'_>,
) -> bool {
    use crate::metal::firmware::acpi::Processor;

    let features = __cpuid(1);
    const MSR_AND_APIC: u32 = (1 << 5) | (1 << 9);
    if features.edx & MSR_AND_APIC != MSR_AND_APIC {
        return false;
    }

    let (low, high): (u32, u32);
    // The admitted boot entry is already at CPL0 with interrupts masked.
    unsafe {
        asm!("rdmsr", in("ecx") 0x1bu32, out("eax") low, out("edx") high,
            options(nomem, nostack, preserves_flags));
    }
    let base = (u64::from(high) << 32) | u64::from(low);
    const ENABLED_MAJOR: u64 = (1 << 8) | (1 << 11);
    const X2APIC: u64 = 1 << 10;
    const APIC_ADDRESS: u64 = 0x000f_ffff_ffff_f000;
    if base & ENABLED_MAJOR != ENABLED_MAJOR
        || base & X2APIC != 0
        || !madt
            .local_controller_address()
            .is_some_and(|address| address.get() == base & APIC_ADDRESS)
    {
        return false;
    }

    let mut matching = madt.processors().filter(|processor| match processor {
        Processor::Apic { id, .. } => usize::from(*id) == hart,
        _ => false,
    });
    matching.next().is_some_and(|processor| processor.enabled()) && matching.next().is_none()
}

const GS_BASE: u32 = 0xc000_0101;

pub(super) fn local_address() -> usize {
    let (low, high): (u32, u32);
    unsafe {
        asm!("rdmsr", in("ecx") GS_BASE, out("eax") low, out("edx") high, options(nostack));
    }
    ((high as usize) << 32) | low as usize
}

pub(super) unsafe fn install_local(address: usize) {
    unsafe {
        asm!("wrmsr", in("ecx") GS_BASE, in("eax") address as u32,
            in("edx") (address >> 32) as u32, options(nostack));
    }
}

pub(super) unsafe fn mask_interrupts() -> usize {
    let previous: usize;
    unsafe { asm!("pushfq", "cli", "pop {}", out(reg) previous) };
    previous & (1 << 9)
}

pub(super) unsafe fn restore_interrupts(previous: usize) {
    if previous != 0 {
        unsafe { asm!("sti", options(nostack)) };
    }
}

pub(crate) fn stack_pointer() -> usize {
    let sp: usize;
    unsafe { asm!("mov {}, rsp", out(reg) sp, options(nomem, nostack)) };
    sp
}
