use core::arch::{asm, x86_64::__cpuid};

pub(crate) fn current_hart() -> usize {
    (__cpuid(1).ebx >> 24) as usize
}

pub(crate) fn stack_pointer() -> usize {
    let sp: usize;
    unsafe { asm!("mov {}, rsp", out(reg) sp, options(nomem, nostack)) };
    sp
}
