//! This optional link-wrapped probe observes the production switch boundary.
use core::{
    arch::global_asm,
    sync::atomic::{AtomicUsize, Ordering},
};

global_asm!(
    r#"
# Interpose exactly at the production switch boundary. Rust frames may use
# s-registers themselves, so setting them one caller earlier misses saves.
.section .text.execution_probe,"ax"
.balign 4
.global __wrap_limen_switch_context
.type __wrap_limen_switch_context, @function
__wrap_limen_switch_context:
    addi sp, sp, -128
    sd ra, 0(sp)
    .irp index,0,1,2,3,4,5,6,7,8,9,10,11
    sd s\index, 8*(1+\index)(sp)
    .endr
    sd tp, 112(sp)

    # Each Unit and each dispatcher has a distinct saved-frame address.
    # Shift it to leave space for twelve distinct register values.
    slli t0, a0, 4
    sd t0, 104(sp)
    .irp index,0,1,2,3,4,5,6,7,8,9,10,11
    addi s\index, t0, \index
    .endr

    call __real_limen_switch_context

    ld t0, 104(sp)
    .irp index,0,1,2,3,4,5,6,7,8,9,10,11
    li t2, \index
    addi t1, t0, \index
    bne s\index, t1, .Lmismatch
    .endr
    ld t0, 112(sp)
    beq tp, t0, .Lrestore
    la t0, limen_register_probe_migrated
    li t1, 1
    amoadd.d zero, t1, (t0)

.Lrestore:
    .irp index,0,1,2,3,4,5,6,7,8,9,10,11
    ld s\index, 8*(1+\index)(sp)
    .endr
    ld ra, 0(sp)
    addi sp, sp, 128
    ret

.Lmismatch:
    mv a0, t2
    tail limen_register_probe_mismatch
.size __wrap_limen_switch_context, .-__wrap_limen_switch_context
"#
);

#[unsafe(no_mangle)]
static limen_register_probe_migrated: AtomicUsize = AtomicUsize::new(0);

pub fn migrations() -> usize {
    limen_register_probe_migrated.load(Ordering::Relaxed)
}

#[unsafe(no_mangle)]
extern "C" fn limen_register_probe_mismatch(register: usize) -> ! {
    panic!("kernel-execution-harts: register s{register} changed");
}
