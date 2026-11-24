use core::arch::asm;

mod start;

pub(crate) use super::firmware::sbi::services;
mod minor;
pub(crate) use minor::{MinorContext, minor_context, wake_minor};
pub(crate) const REQUIRES_DIAGNOSTICS: bool = true;

#[repr(C, align(16))]
struct TrapStack([u8; 4096]);

#[unsafe(link_section = ".limen.trap.stack")]
static mut TRAP_STACK: TrapStack = TrapStack([0; 4096]);

/// Checks the state established by the assembly entry without changing translation.
pub(crate) fn established() -> bool {
    established_on(primary_trap_stack())
}

pub(crate) fn primary_trap_stack() -> usize {
    unsafe { (&raw const TRAP_STACK.0).addr() + size_of::<TrapStack>() }
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
        && vector == start::trap as *const () as usize
        && scratch == trap_stack
}

pub(crate) fn image() -> core::ops::Range<usize> {
    // These boundaries describe the final linked image, including the platform
    // sections supplied by its consumer. No Rust object spans that interval.
    unsafe extern "C" {
        static _kernel_base: u8;
        static _kernel_edge: u8;
    }
    (&raw const _kernel_base).addr()..(&raw const _kernel_edge).addr()
}

pub(crate) fn park() -> ! {
    loop {
        unsafe { asm!("wfi", options(nomem, nostack)) };
    }
}
pub use minor::Startup;
