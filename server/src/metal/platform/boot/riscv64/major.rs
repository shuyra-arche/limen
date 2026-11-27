//! OpenSBI enters the major CPU with (hart, DTB).
use crate::metal::platform::boot::treaty::MajorMaterial;
use crate::metal::processor::{
    description::riscv64::status,
    execution::terminal::{riscv64::trap, trap as diagnostic},
    executor::entry::riscv64::establish_supervisor,
};
use core::arch::naked_asm;

unsafe extern "Rust" {
    static LIMEN_MAJOR_MATERIAL: MajorMaterial;
}

// OpenSBI hands this entry to one hart. Firmware holds the other harts until
// HSM startup, which the platform can request only after this handoff.
#[unsafe(naked)]
#[unsafe(export_name = "_start")]
#[unsafe(link_section = ".text._start")]
unsafe extern "C" fn major(hart: usize, dtb: usize) -> ! {
    naked_asm!(
        establish_supervisor!(),
        "la sp, _major_stack_end",
        "la t0, {top}",
        "csrw sscratch, t0",
        "la t0, {vector}",
        "csrw stvec, t0",
        // Both stacks may be in BSS. Only registers are used until the clear
        // finishes. A terminal trap takes the emergency stack and cannot
        // return to the clear, so its live frames never meet the zeroing loop.
        "la t0, _bss_start",
        "la t1, _bss_end",
        "2: beq t0, t1, 3f",
        "sd zero, 0(t0)",
        "addi t0, t0, 8",
        "j 2b",
        "3: mv a2, sp",
        "la a3, {material}",
        "tail {loaded}",
        interrupt_enable = const status::SIE,
        extensions = const status::WRITABLE_EXTENSIONS,
        material = sym LIMEN_MAJOR_MATERIAL,
        top = sym diagnostic::TOP,
        vector = sym trap,
        loaded = sym crate::entry::boot::start,
    );
}
