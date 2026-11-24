.macro establish_supervisor
    csrci sstatus, 2
    csrw sie, zero
    # Fresh supervisor code has no saved FP or vector context.
    li t0, (3 << 13) | (3 << 9)
    csrc sstatus, t0
    .option push
    .option norelax
    la gp, __global_pointer$
    .option pop
    mv tp, a0
.endm

.section .text._start,"ax"
.balign 4
.globl _start
_start:
    establish_supervisor
    la sp, {boot_stack}
    li t0, {boot_bytes}
    add sp, sp, t0
    la t0, {trap_stack}
    li t1, {trap_bytes}
    add t0, t0, t1
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

.section .text.secondary_boot,"ax"
.balign 4
.globl _limen_secondary_start
_limen_secondary_start:
    establish_supervisor
    csrw sscratch, zero
    la t0, _limen_early_trap
    csrw stvec, t0

    # Acquire the launch descriptor before reading its stack addresses.
    addi t0, a1, {secondary_state}
    li t1, {secondary_published}
    li t2, {secondary_entered}
4:
    lr.w.aq t3, (t0)
    bne t3, t1, 5f
    sc.w t3, t2, (t0)
    bnez t3, 4b

    ld t0, {secondary_hart}(a1)
    bne a0, t0, 5f
    ld sp, {secondary_stack}(a1)
    ld t0, {secondary_trap_stack}(a1)
    csrw sscratch, t0
    tail limen_secondary_boot
5:
    wfi
    j 5b

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
