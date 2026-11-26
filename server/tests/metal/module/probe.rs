#![feature(no_core, lang_items, rustc_attrs)]
#![no_core]
#![allow(internal_features)]

// The fixtures need the pinned integrated assembler on three targets.
// They can be built on a Host worker before any target core is installed.
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

#[cfg(target_arch = "riscv64")]
global_asm!(
    r#"
.section .text.module_geometry, "ax", %progbits
.p2align 2
.global module_geometry
.hidden module_geometry
.type module_geometry, %function
module_geometry:
    csrr a0, satp
    srli a0, a0, 60
    li t0, 8
    bne a0, t0, 1f
    lla a0, geometry_pointer
    ld a0, 0(a0)
    ld a0, 0(a0)
    ret
1:
    li a0, 0
    ret
.size module_geometry, . - module_geometry
"#
);

#[cfg(target_arch = "x86_64")]
global_asm!(
    r#"
.section .text.module_geometry, "ax", %progbits
.p2align 2
.global module_geometry
.hidden module_geometry
.type module_geometry, %function
module_geometry:
    mov rax, cr0
    bt rax, 31
    jnc 1f
    mov rax, cr4
    test eax, 1 << 5
    jz 1f
    mov rax, qword ptr [rip + geometry_pointer]
    mov rax, qword ptr [rax]
    ret
1:
    xor eax, eax
    ret
.size module_geometry, . - module_geometry
"#
);

#[cfg(target_arch = "aarch64")]
global_asm!(
    r#"
.section .text.module_geometry, "ax", %progbits
.p2align 2
.global module_geometry
.hidden module_geometry
.type module_geometry, %function
module_geometry:
    mrs x0, SCTLR_EL1
    tbz x0, 0, 1f
    mrs x0, TCR_EL1
    tbnz x0, 14, 1f
    tbnz x0, 15, 1f
    adrp x0, geometry_pointer
    ldr x0, [x0, :lo12:geometry_pointer]
    ldr x0, [x0]
    ret
1:
    mov x0, #0
    ret
.size module_geometry, . - module_geometry
"#
);

global_asm!(
    r#"
.section .rodata.geometry, "a", %progbits
.p2align 3
geometry:
    .quad 4096

.section .data.geometry, "aw", %progbits
.p2align 3
geometry_pointer:
    .quad geometry

.section .bss.geometry, "aw", %nobits
.p2align 3
    .zero 32
"#
);
