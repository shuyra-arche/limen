//! OpenSBI enters the major CPU with (hart, DTB).
use super::trap;
use crate::metal::generic::boot::trap as diagnostic;
use crate::metal::generic::boot::{STACK_BYTES, major::STACK};
use core::arch::naked_asm;

#[unsafe(naked)]
#[unsafe(export_name = "_start")]
#[unsafe(link_section = ".text._start")]
unsafe extern "C" fn major(hart: usize, dtb: usize) -> ! {
    naked_asm!(
        establish_supervisor!(),
        "la sp, {stack}",
        "li t0, {size}",
        "add sp, sp, t0",
        "la t0, {trap_stack}",
        "li t1, {trap_size}",
        "add t0, t0, t1",
        "csrw sscratch, t0",
        "la t0, {vector}",
        "csrw stvec, t0",
        "la t0, _bss_start",
        "la t1, _bss_end",
        "2: beq t0, t1, 3f",
        "sd zero, 0(t0)",
        "addi t0, t0, 8",
        "j 2b",
        "3: tail {loaded}",
        stack = sym STACK,
        size = const STACK_BYTES,
        trap_stack = sym diagnostic::STACK,
        trap_size = const diagnostic::BYTES,
        vector = sym trap,
        loaded = sym crate::entry::boot::start,
    );
}
