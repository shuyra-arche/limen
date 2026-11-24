//! OpenSBI enters the primary with (hart, DTB) and a secondary with (hart, descriptor).
use super::{TRAP_STACK, TrapStack, firmware, secondary};
use crate::metal::stack::{STACK, Stack};
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

#[unsafe(naked)]
#[unsafe(export_name = "_start")]
#[unsafe(link_section = ".text._start")]
unsafe extern "C" fn enter(hart: usize, dtb: usize) -> ! {
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
        loaded = sym firmware::limen_boot,
    );
}

#[unsafe(naked)]
#[unsafe(export_name = "_limen_secondary_start")]
pub(super) unsafe extern "C" fn enter_secondary(hart: usize, descriptor: usize) -> ! {
    naked_asm!(
        establish_supervisor!(),
        "csrw sscratch, zero",
        "la t0, {vector}",
        "csrw stvec, t0",
        // Acquire the published descriptor before reading its stack addresses.
        "addi t0, a1, {state}",
        "li t1, {published}",
        "li t2, {entered}",
        "2: lr.w.aq t3, (t0)",
        "bne t3, t1, 3f",
        "sc.w t3, t2, (t0)",
        "bnez t3, 2b",
        "ld t0, {hart}(a1)",
        "bne a0, t0, 3f",
        "ld sp, {stack}(a1)",
        "ld t0, {trap_stack}(a1)",
        "csrw sscratch, t0",
        "tail {loaded}",
        "3: wfi",
        "j 3b",
        vector = sym trap,
        state = const secondary::STATE_OFFSET,
        published = const secondary::PUBLISHED,
        entered = const secondary::ENTERED,
        hart = const secondary::HART_OFFSET,
        stack = const secondary::STACK_OFFSET,
        trap_stack = const secondary::TRAP_STACK_OFFSET,
        loaded = sym secondary::limen_secondary_boot,
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
        report = sym firmware::limen_boot_trap,
    );
}
