//! OpenSBI enters the primary with (hart, DTB) and a secondary with (hart, descriptor).
use super::{STACK, TRAP_STACK, TrapStack};
use crate::metal::generic::stack::Stack;
use core::arch::naked_asm;

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

pub(super) use establish_supervisor;

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
        size = const size_of::<Stack>(),
        trap_stack = sym TRAP_STACK,
        trap_size = const size_of::<TrapStack>(),
        vector = sym trap,
        loaded = sym crate::entry::boot::start,
    );
}

#[unsafe(naked)]
#[rustc_align(4)]
#[unsafe(export_name = "_limen_early_trap")]
pub(super) unsafe extern "C" fn trap() -> ! {
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
