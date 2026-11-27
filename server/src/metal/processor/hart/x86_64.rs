use core::arch::{asm, x86_64::__cpuid};

pub(crate) fn boot_identity() -> usize {
    (__cpuid(1).ebx >> 24) as usize
}

const GS_BASE: u32 = 0xc000_0101;

pub(in crate::metal::processor::hart) fn local_address() -> usize {
    let (low, high): (u32, u32);
    unsafe {
        asm!("rdmsr", in("ecx") GS_BASE, out("eax") low, out("edx") high, options(nostack));
    }
    ((high as usize) << 32) | low as usize
}

pub(in crate::metal::processor::hart) unsafe fn install_local(address: usize) {
    unsafe {
        asm!("wrmsr", in("ecx") GS_BASE, in("eax") address as u32,
            in("edx") (address >> 32) as u32, options(nostack));
    }
}

pub(in crate::metal::processor::hart) unsafe fn mask_interrupts() -> usize {
    let previous: usize;
    unsafe { asm!("pushfq", "cli", "pop {}", out(reg) previous) };
    previous & (1 << 9)
}

pub(in crate::metal::processor::hart) unsafe fn restore_interrupts(previous: usize) {
    if previous != 0 {
        unsafe { asm!("sti", options(nostack)) };
    }
}

pub(crate) fn stack_pointer() -> usize {
    let sp: usize;
    unsafe { asm!("mov {}, rsp", out(reg) sp, options(nomem, nostack)) };
    sp
}

/// Clear the inherited local register before installing the trusted entry.
pub(in crate::metal) unsafe fn clear_local() {
    unsafe { install_local(0) };
}
