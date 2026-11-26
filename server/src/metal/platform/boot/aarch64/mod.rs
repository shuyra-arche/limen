use crate::metal::{
    platform::boot::handoff::{Descriptions, Facts, uefi},
    platform::boot::trap as diagnostic,
    processor::hart::aarch64::native,
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
    let (daif, spsel): (usize, usize);
    unsafe {
        asm!("mrs {}, DAIF", "mrs {}, SPSel", out(reg) daif, out(reg) spsel,
             options(nostack, nomem, preserves_flags));
    }
    daif & 0x3c0 == 0x3c0 && spsel == 1 && entry_installed()
}

pub(crate) fn entry_installed() -> bool {
    let el: usize;
    unsafe { asm!("mrs {}, CurrentEL", out(reg) el, options(nostack, nomem, preserves_flags)) };
    if el != 4 {
        return false;
    }
    let vector: usize;
    unsafe { asm!("mrs {}, VBAR_EL1", out(reg) vector, options(nostack, nomem, preserves_flags)) };
    vector == vectors as *const () as usize
}
pub(crate) fn park() -> ! {
    loop {
        unsafe { asm!("wfe", options(nomem, nostack)) };
    }
}

mod minor;
pub use minor::Startup;
pub(crate) use minor::{MinorContext, minor_context, wake_minor};

pub type DeliveryError = core::convert::Infallible;

#[unsafe(naked)]
#[rustc_align(2048)]
unsafe extern "C" fn vectors() -> ! {
    naked_asm!(
        ".set slot, 0",
        ".rept 16",
        "mov x0, #slot",
        "b 2f",
        ".balign 128",
        ".set slot, slot + 1",
        ".endr",
        "2: msr daifset, #15",
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
