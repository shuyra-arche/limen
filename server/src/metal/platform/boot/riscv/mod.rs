pub(crate) use super::handoff::sbi::admit;
use crate::metal::{
    interrupt::internal::supervisor,
    memory::mmu::internal::riscv::context::Context,
    platform::boot::trap as diagnostic,
    processor::hart::{local, riscv::{native, status}},
};
use core::arch::{asm, naked_asm};

// Both entries establish supervisor state before using either stack.
macro_rules! establish_supervisor {
    () => {
        r#"
        csrci sstatus, {interrupt_enable}
        csrw sie, zero
        li t0, {extensions}
        csrc sstatus, t0
        .option push
        .option norelax
        la gp, __global_pointer$
        .option pop
        mv tp, zero
        "#
    };
}

mod major;
mod minor;
pub use minor::Startup;
pub(crate) use minor::{MinorContext, minor_context, wake_minor};

pub(crate) fn established() -> bool {
    let status = native::status();
    status.masked()
        && status.extensions_off()
        && supervisor::all_masked()
        && Context::current() == Some(Context::BARE)
        && entry().is_some()
}

pub(in crate::metal) fn entry() -> Option<local::Entry> {
    local::Entry::admit(
        native::Entry {
            vector: trap as *const () as usize,
            scratch: (&raw const diagnostic::TOP).addr(),
        },
        diagnostic::stack_range(),
    )
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
