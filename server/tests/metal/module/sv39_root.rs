#![feature(no_core, lang_items, rustc_attrs)]
#![no_core]
#![allow(internal_features)]

#[lang = "pointee_sized"]
pub trait PointeeSized {}
#[lang = "meta_sized"]
pub trait MetaSized: PointeeSized {}
#[lang = "sized"]
pub trait Sized: MetaSized {}

#[rustc_builtin_macro]
macro_rules! global_asm {
    ($($tokens:tt)*) => {};
}

// Internal Sv39Root v1 (RV64 ELF C ABI). The input table is GeometryV1:
// version at 0, byte length at 4, and the previously bound query at 8.
// The second and third arguments are the physical byte address and encoding
// kind. Firmware and the host loader retain custody of the target tables.
global_asm!(
    r#"
.section .text.module_sv39_root, "ax", %progbits
.p2align 2
.global module_sv39_root
.hidden module_sv39_root
.type module_sv39_root, %function
module_sv39_root:
    beqz a0, .Lreject
    andi t0, a0, 7
    bnez t0, .Lreject
    lwu t0, 0(a0)
    li t1, 1
    bne t0, t1, .Lreject
    lwu t0, 4(a0)
    li t1, 16
    bne t0, t1, .Lreject
    ld t0, 8(a0)
    beqz t0, .Lreject
    andi t1, t0, 1
    bnez t1, .Lreject
    srli t1, a1, 56
    bnez t1, .Lreject
    li t1, 4
    bgtu a2, t1, .Lreject

    # Call the actual published provider. It can clobber every a- and
    # t-register, so preserve both operation inputs and our return address.
    addi sp, sp, -32
    sd ra, 24(sp)
    sd a1, 16(sp)
    sd a2, 8(sp)
    jalr ra, t0, 0
    ld a2, 8(sp)
    ld a1, 16(sp)
    ld ra, 24(sp)
    addi sp, sp, 32

    li t0, 4096
    bne a0, t0, .Lreject
    addi t0, a0, -1
    and t0, a1, t0
    bnez t0, .Lreject
    srli a1, a1, 12
    li t0, 4
    beq a2, t0, .Lcontext

    lla t0, .Lflags
    add t0, t0, a2
    lbu t0, 0(t0)
    slli a0, a1, 10
    or a0, a0, t0
    ret
.Lcontext:
    li a0, 8
    slli a0, a0, 60
    or a0, a0, a1
    ret
.Lreject:
    li a0, 0
    ret
.size module_sv39_root, . - module_sv39_root

.section .rodata.sv39_root, "a", %progbits
.Lflags:
    # Nonleaf, R, RW, RX. Eagerly set A and D only on the applicable leaves.
    .byte 0x01, 0x43, 0xc7, 0x4b
"#
);
