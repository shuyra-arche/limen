use core::arch::{asm, global_asm};

mod firmware;
mod secondary;

pub use secondary::Secondary;

global_asm!(
    include_str!("start.S"),
    secondary_state = const secondary::STATE_OFFSET,
    secondary_published = const secondary::PUBLISHED,
    secondary_entered = const secondary::ENTERED,
    secondary_hart = const secondary::HART_OFFSET,
    secondary_stack = const secondary::STACK_OFFSET,
    secondary_trap_stack = const secondary::TRAP_STACK_OFFSET,
);

unsafe extern "C" {
    static _boot_stack_start: u8;
    static _boot_stack_end: u8;
    static _kernel_base: u8;
    static _kernel_edge: u8;
    static _limen_early_trap: u8;
    static _trap_stack_end: u8;
}

/// Memory reserved by the linker for the bootstrap CPU's ordinary stack.
pub fn stack_range() -> core::ops::Range<usize> {
    (&raw const _boot_stack_start).addr()..(&raw const _boot_stack_end).addr()
}

/// Checks the state established by the assembly entry without changing translation.
pub(crate) fn established() -> bool {
    established_on(primary_trap_stack())
}

pub(crate) fn primary_trap_stack() -> usize {
    (&raw const _trap_stack_end).addr()
}

/// Trusted firmware identity retained in the kernel's ABI-reserved register.
pub(crate) fn current_hart() -> usize {
    let hart;
    // A resumed continuation may run on another hart, so keep this read
    // ordered with calls that transfer its context.
    unsafe { asm!("mv {}, tp", out(reg) hart, options(nostack)) };
    hart
}

pub(crate) fn established_on(trap_stack: usize) -> bool {
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
        && scratch == trap_stack
}

pub(crate) fn image() -> core::ops::Range<usize> {
    (&raw const _kernel_base).addr()..(&raw const _kernel_edge).addr()
}

pub(crate) fn park() -> ! {
    loop {
        unsafe { asm!("wfi", options(nomem, nostack)) };
    }
}
