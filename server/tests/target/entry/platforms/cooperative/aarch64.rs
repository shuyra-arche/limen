//! End-to-end AAPCS64 nonvolatile register and control checks through Yield.

use core::arch::{asm, naked_asm};
use kernel::basis::execution::Current;

pub(super) fn control() -> u64 {
    let value: u64;
    unsafe {
        asm!("mrs {}, FPCR", out(reg) value, options(nomem, nostack, preserves_flags));
    }
    value
}

pub(super) fn rounding(value: u64) -> u64 {
    value ^ (1 << 22)
}

pub(super) fn install(value: u64) {
    unsafe {
        asm!("msr FPCR, {}", "isb", in(reg) value, options(nostack, preserves_flags));
    }
}

pub(super) fn verify(current: &mut Current) {
    unsafe { probe(current) };
}

extern "C" fn yield_now(current: *mut Current) {
    unsafe { &mut *current }.yield_now().expect("AAPCS64 yield");
}

#[unsafe(naked)]
unsafe extern "C" fn probe(current: *mut Current) {
    naked_asm!(
        "sub sp, sp, #224",
        "stp x19, x20, [sp, #0]",
        "stp x21, x22, [sp, #16]",
        "stp x23, x24, [sp, #32]",
        "stp x25, x26, [sp, #48]",
        "stp x27, x28, [sp, #64]",
        "stp x29, x30, [sp, #80]",
        "stp q8, q9, [sp, #96]",
        "stp q10, q11, [sp, #128]",
        "stp q12, q13, [sp, #160]",
        "stp q14, q15, [sp, #192]",
        "mov x19, #19",
        "mov x20, #20",
        "mov x21, #21",
        "mov x22, #22",
        "mov x23, #23",
        "mov x24, #24",
        "mov x25, #25",
        "mov x26, #26",
        "mov x27, #27",
        "mov x28, #28",
        "mov x29, #29",
        "movi v8.8b, #8",
        "movi v9.8b, #9",
        "movi v10.8b, #10",
        "movi v11.8b, #11",
        "movi v12.8b, #12",
        "movi v13.8b, #13",
        "movi v14.8b, #14",
        "movi v15.8b, #15",
        "bl {yield_now}",
        "cmp x19, #19",
        "b.ne 2f",
        "cmp x20, #20",
        "b.ne 2f",
        "cmp x21, #21",
        "b.ne 2f",
        "cmp x22, #22",
        "b.ne 2f",
        "cmp x23, #23",
        "b.ne 2f",
        "cmp x24, #24",
        "b.ne 2f",
        "cmp x25, #25",
        "b.ne 2f",
        "cmp x26, #26",
        "b.ne 2f",
        "cmp x27, #27",
        "b.ne 2f",
        "cmp x28, #28",
        "b.ne 2f",
        "cmp x29, #29",
        "b.ne 2f",
        "fmov x9, d8",
        "movi v0.8b, #8",
        "fmov x10, d0",
        "cmp x9, x10",
        "b.ne 2f",
        "fmov x9, d9",
        "movi v0.8b, #9",
        "fmov x10, d0",
        "cmp x9, x10",
        "b.ne 2f",
        "fmov x9, d10",
        "movi v0.8b, #10",
        "fmov x10, d0",
        "cmp x9, x10",
        "b.ne 2f",
        "fmov x9, d11",
        "movi v0.8b, #11",
        "fmov x10, d0",
        "cmp x9, x10",
        "b.ne 2f",
        "fmov x9, d12",
        "movi v0.8b, #12",
        "fmov x10, d0",
        "cmp x9, x10",
        "b.ne 2f",
        "fmov x9, d13",
        "movi v0.8b, #13",
        "fmov x10, d0",
        "cmp x9, x10",
        "b.ne 2f",
        "fmov x9, d14",
        "movi v0.8b, #14",
        "fmov x10, d0",
        "cmp x9, x10",
        "b.ne 2f",
        "fmov x9, d15",
        "movi v0.8b, #15",
        "fmov x10, d0",
        "cmp x9, x10",
        "b.ne 2f",
        "ldp q8, q9, [sp, #96]",
        "ldp q10, q11, [sp, #128]",
        "ldp q12, q13, [sp, #160]",
        "ldp q14, q15, [sp, #192]",
        "ldp x19, x20, [sp, #0]",
        "ldp x21, x22, [sp, #16]",
        "ldp x23, x24, [sp, #32]",
        "ldp x25, x26, [sp, #48]",
        "ldp x27, x28, [sp, #64]",
        "ldp x29, x30, [sp, #80]",
        "add sp, sp, #224",
        "ret",
        "2: brk #0",
        yield_now = sym yield_now,
    )
}
