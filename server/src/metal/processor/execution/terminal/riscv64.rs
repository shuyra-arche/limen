use super::trap as diagnostic;
use core::arch::{asm, naked_asm};

#[unsafe(naked)]
#[rustc_align(4)]
pub(in crate::metal) unsafe extern "C" fn trap() -> ! {
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
