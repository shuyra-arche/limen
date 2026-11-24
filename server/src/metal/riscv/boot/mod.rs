use core::arch::{asm, global_asm};

global_asm!(include_str!("start.S"));

unsafe extern "C" {
    static _kernel_base: u8;
    static _kernel_edge: u8;
    static _limen_early_trap: u8;
    static _trap_stack_end: u8;
}

/// Checks the state established by the assembly entry without changing translation.
pub(crate) fn established() -> bool {
    let (status, interrupt_enable, translation, vector, scratch): (
        usize,
        usize,
        usize,
        usize,
        usize,
    );
    unsafe {
        asm!(
            "csrr {status}, sstatus",
            "csrr {interrupt_enable}, sie",
            "csrr {translation}, satp",
            "csrr {vector}, stvec",
            "csrr {scratch}, sscratch",
            status = out(reg) status,
            interrupt_enable = out(reg) interrupt_enable,
            translation = out(reg) translation,
            vector = out(reg) vector,
            scratch = out(reg) scratch,
            options(nomem, nostack),
        );
    }
    status & 2 == 0
        && interrupt_enable == 0
        && translation == 0
        && vector == (&raw const _limen_early_trap).addr()
        && scratch == (&raw const _trap_stack_end).addr()
}

pub(crate) fn image() -> core::ops::Range<usize> {
    (&raw const _kernel_base).addr()..(&raw const _kernel_edge).addr()
}

pub(crate) fn park() -> ! {
    loop {
        unsafe { asm!("wfi", options(nomem, nostack)) };
    }
}
