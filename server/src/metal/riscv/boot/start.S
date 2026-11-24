.section .text._start,"ax"
.balign 4
.globl _start
_start:
    csrci sstatus, 2
    csrw sie, zero
    .option push
    .option norelax
    la gp, __global_pointer$
    .option pop
    la sp, _boot_stack_end
    la t0, _trap_stack_end
    csrw sscratch, t0
    la t0, _limen_early_trap
    csrw stvec, t0

    la t0, _bss_start
    la t1, _bss_end
1:
    beq t0, t1, 2f
    sd zero, 0(t0)
    addi t0, t0, 8
    j 1b
2:
    tail limen_boot

.section .text.boot,"ax"
.balign 4
.globl _limen_early_trap
_limen_early_trap:
    csrrw sp, sscratch, sp
    beqz sp, 3f
    csrr a3, sscratch
    csrw sscratch, zero
    csrr a0, scause
    csrr a1, sepc
    csrr a2, stval
    tail limen_boot_trap
3:
    wfi
    j 3b
