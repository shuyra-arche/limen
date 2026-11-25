use crate::metal::boot::trap as diagnostic;
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
pub use minor::Startup;
pub(crate) use minor::{MinorContext, minor_context, wake_minor};

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
        && vector == trap as *const () as usize
        && scratch == (&raw const diagnostic::TOP).addr()
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

pub type DeliveryError = core::convert::Infallible;

#[unsafe(naked)]
#[rustc_align(4)]
unsafe extern "C" fn trap() -> ! {
    naked_asm!(
        "csrr t0, sscratch",
        "amoswap.d.aq t0, zero, (t0)",
        "beqz t0, 2f",
        "mv a0, sp",
        "mv sp, t0",
        "tail {capture}",
        "2: wfi",
        "j 2b",
        capture = sym capture,
    );
}

extern "C" fn capture(stack: usize) -> ! {
    let (cause, pc, value, status);
    unsafe {
        asm!(
            "csrr {}, scause", "csrr {}, sepc", "csrr {}, stval", "csrr {}, sstatus",
            out(reg) cause, out(reg) pc, out(reg) value, out(reg) status, options(nomem, nostack)
        )
    }
    diagnostic::Trap {
        cause,
        pc,
        value,
        stack,
        status,
    }
    .report()
}
