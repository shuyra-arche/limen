use crate::metal::generic::stack::Stack;
use core::arch::{asm, naked_asm};

// Both entries establish supervisor state before using either stack.
macro_rules! establish_supervisor {
    () => {
        r#"
        csrci sstatus, 2
        csrw sie, zero
        li t0, (3 << 13) | (3 << 9)
        csrc sstatus, t0
        .option push
        .option norelax
        la gp, __global_pointer$
        .option pop
        mv tp, a0
        "#
    };
}

mod major;
mod minor;
pub(crate) use minor::{MinorContext, minor_context, wake_minor};

pub(crate) type TrapStack = Stack<4096>;

#[unsafe(link_section = ".limen.trap.stack")]
static mut TRAP_STACK: TrapStack = TrapStack::new();

/// Checks the state established by the assembly entry without changing translation.
pub(crate) fn established() -> bool {
    established_on(primary_trap_stack())
}

pub(crate) fn primary_trap_stack() -> usize {
    (&raw const TRAP_STACK).addr() + size_of::<TrapStack>()
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
        && vector == trap as *const () as usize
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
pub(crate) use crate::metal::generic::firmware::sbi as firmware;
pub(crate) mod interface {
    pub use super::minor::Startup;
}

pub(crate) const STACK_BYTES: usize = 16 * 1024;
#[unsafe(link_section = ".limen.boot.stack")]
pub(in crate::metal) static mut STACK: Stack<STACK_BYTES> = Stack::new();

pub(crate) fn stack_pointer() -> usize {
    let sp: usize;
    unsafe { asm!("mv {}, sp", out(reg) sp, options(nomem, nostack)) };
    sp
}

pub type DeliveryError = core::convert::Infallible;

#[unsafe(naked)]
#[rustc_align(4)]
unsafe extern "C" fn trap() -> ! {
    naked_asm!(
        "csrrw sp, sscratch, sp",
        "beqz sp, 2f",
        "csrr a3, sscratch",
        "csrw sscratch, zero",
        "csrr a0, scause",
        "csrr a1, sepc",
        "csrr a2, stval",
        "tail {report}",
        "2: wfi",
        "j 2b",
        report = sym report_trap,
    );
}

extern "C" fn report_trap(cause: usize, pc: usize, value: usize, stack: usize) -> ! {
    crate::metal::generic::firmware::sbi::services().reject(format_args!(
        "FAIL Limen boot trap: cause={cause:#x} pc={pc:#x} value={value:#x} stack={stack:#x}\n"
    ))
}
