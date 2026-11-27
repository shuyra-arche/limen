use crate::metal::{
    platform::boot::handoff::{Descriptions, Facts, uefi},
    platform::boot::trap as diagnostic,
    processor::{
        description::aarch64::status,
        executor::{aarch64::native, local},
    },
};
mod major;
pub(crate) use major::major as enter_major;

use core::arch::{asm, naked_asm};

pub(crate) unsafe fn admit<'a>(
    hart: usize,
    input: *const (),
    descriptions: &mut Descriptions<'a>,
) -> Option<Facts<'a>> {
    unsafe { uefi::admit(hart, native::boot_identity(), input, descriptions) }
}

pub(crate) fn established() -> bool {
    native::masked_stack() && entry().is_some()
}

pub(in crate::metal) fn entry() -> Option<local::Entry> {
    local::Entry::admit(
        native::Entry {
            vector: vectors as *const () as usize,
        },
        diagnostic::stack_range(),
    )
}

#[unsafe(naked)]
#[rustc_align(2048)]
pub(in crate::metal) unsafe extern "C" fn vectors() -> ! {
    naked_asm!(
        ".set slot, 0",
        ".rept 16",
        "mov x0, #slot",
        "b 2f",
        ".balign 128",
        ".set slot, slot + 1",
        ".endr",
        "2: msr daifset, #{exceptions}",
        "adrp x2, {top}",
        "add x2, x2, :lo12:{top}",
        "3: ldaxr x3, [x2]",
        "cbz x3, 4f",
        "stxr w4, xzr, [x2]",
        "cbnz w4, 3b",
        "mov x1, sp",
        "mov sp, x3",
        "b {capture}",
        "4: wfe",
        "b 4b",
        exceptions = const status::ALL_EXCEPTIONS,
        top = sym diagnostic::TOP,
        capture = sym capture,
    );
}

extern "C" fn capture(slot: usize, old_sp_el1: usize) -> ! {
    let pc;
    unsafe { asm!("mrs {}, elr_el1", out(reg) pc, options(nomem, nostack)) };
    let stack = if slot / 4 == 1 {
        old_sp_el1
    } else {
        let sp_el0;
        unsafe { asm!("mrs {}, sp_el0", out(reg) sp_el0, options(nomem, nostack)) };
        sp_el0
    };

    // Only synchronous exceptions and SError supply an applicable syndrome.
    let mut status = 0;
    let mut value = 0;
    if matches!(slot % 4, 0 | 3) {
        unsafe { asm!("mrs {}, esr_el1", out(reg) status, options(nomem, nostack)) };
        let exception_class = status >> 26 & 0x3f;
        if matches!(exception_class, 0x20 | 0x21 | 0x24 | 0x25) && status & (1 << 10) == 0 {
            unsafe { asm!("mrs {}, far_el1", out(reg) value, options(nomem, nostack)) };
        }
    }
    diagnostic::Trap {
        cause: slot,
        pc,
        value,
        stack,
        status,
    }
    .report()
}
