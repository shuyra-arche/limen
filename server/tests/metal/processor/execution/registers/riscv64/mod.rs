//! Integer psABI observations across a real cooperative Yield.
use core::arch::{asm, naked_asm};
use kernel::basis::execution::Current;

pub(super) fn control() -> (usize, usize, usize) {
    let (status, sources, translation): (usize, usize, usize);
    unsafe {
        asm!(
            "csrr {status}, sstatus",
            "csrr {sources}, sie",
            "csrr {translation}, satp",
            status = out(reg) status,
            sources = out(reg) sources,
            translation = out(reg) translation,
            options(nomem, nostack),
        );
    }
    // The integer profile excludes SIE and active FS, VS, XS state,
    // individual interrupt sources, and non-Bare translation.
    let status = status & ((1 << 1) | (3 << 9) | (3 << 13) | (3 << 15));
    assert_eq!((status, sources, translation), (0, 0, 0));
    (status, sources, translation)
}

pub(super) fn verify(current: &mut Current) {
    unsafe { probe(current) };
}

extern "C" fn yield_now(current: *mut Current) {
    unsafe { &mut *current }.yield_now().expect("RV64 yield");
}

#[unsafe(naked)]
unsafe extern "C" fn probe(current: *mut Current) {
    naked_asm!(
        "addi sp, sp, -112",
        "sd ra, 0(sp)",
        "sd s0, 8(sp)",
        "sd s1, 16(sp)",
        "sd s2, 24(sp)",
        "sd s3, 32(sp)",
        "sd s4, 40(sp)",
        "sd s5, 48(sp)",
        "sd s6, 56(sp)",
        "sd s7, 64(sp)",
        "sd s8, 72(sp)",
        "sd s9, 80(sp)",
        "sd s10, 88(sp)",
        "sd s11, 96(sp)",
        "li s0, 101",
        "li s1, 102",
        "li s2, 103",
        "li s3, 104",
        "li s4, 105",
        "li s5, 106",
        "li s6, 107",
        "li s7, 108",
        "li s8, 109",
        "li s9, 110",
        "li s10, 111",
        "li s11, 112",
        "call {yield_now}",
        "li t0, 101",
        "bne s0, t0, 2f",
        "li t0, 102",
        "bne s1, t0, 2f",
        "li t0, 103",
        "bne s2, t0, 2f",
        "li t0, 104",
        "bne s3, t0, 2f",
        "li t0, 105",
        "bne s4, t0, 2f",
        "li t0, 106",
        "bne s5, t0, 2f",
        "li t0, 107",
        "bne s6, t0, 2f",
        "li t0, 108",
        "bne s7, t0, 2f",
        "li t0, 109",
        "bne s8, t0, 2f",
        "li t0, 110",
        "bne s9, t0, 2f",
        "li t0, 111",
        "bne s10, t0, 2f",
        "li t0, 112",
        "bne s11, t0, 2f",
        "ld s0, 8(sp)",
        "ld s1, 16(sp)",
        "ld s2, 24(sp)",
        "ld s3, 32(sp)",
        "ld s4, 40(sp)",
        "ld s5, 48(sp)",
        "ld s6, 56(sp)",
        "ld s7, 64(sp)",
        "ld s8, 72(sp)",
        "ld s9, 80(sp)",
        "ld s10, 88(sp)",
        "ld s11, 96(sp)",
        "ld ra, 0(sp)",
        "addi sp, sp, 112",
        "ret",
        "2: ebreak",
        yield_now = sym yield_now,
    );
}
