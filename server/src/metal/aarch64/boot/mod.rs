use crate::metal::generic::boot::trap as diagnostic;
mod major;
pub(crate) use major::major as enter_major;

use core::arch::{asm, naked_asm};

pub(crate) fn current_hart() -> usize {
    let id: usize;
    unsafe { asm!("mrs {}, mpidr_el1", out(reg) id, options(nomem,nostack)) };
    id & 0xff00ffffff
}

pub(crate) fn established() -> bool {
    let (el, daif, spsel, vector): (usize, usize, usize, usize);
    unsafe {
        asm!("mrs {}, CurrentEL", "mrs {}, DAIF", "mrs {}, SPSel", "mrs {}, VBAR_EL1",
             out(reg) el, out(reg) daif, out(reg) spsel, out(reg) vector,
             options(nostack, nomem, preserves_flags));
    }
    el == 4 && daif & 0x3c0 == 0x3c0 && spsel == 1 && vector == vectors as *const () as usize
}
pub(crate) fn park() -> ! {
    loop {
        unsafe { asm!("wfe", options(nomem, nostack)) };
    }
}

mod minor;
pub(crate) use crate::metal::generic::firmware::uefi as firmware;
pub(crate) mod interface {
    pub use super::firmware::{Loader, major};
    pub use super::minor::Startup;
    pub use crate::metal::generic::firmware::psci::Conduit;
}
pub(crate) use minor::{MinorContext, minor_context, wake_minor};

pub(crate) fn stack_pointer() -> usize {
    let sp: usize;
    unsafe { asm!("mov {}, sp", out(reg) sp, options(nomem, nostack)) };
    sp
}

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
        "adrp x5, {available}",
        "add x5, x5, :lo12:{available}",
        "3: ldaxr w6, [x5]",
        "cbz w6, 6f",
        "stxr w7, wzr, [x5]",
        "cbnz w7, 3b",
        // SP_EL1 is active on vector entry. Slot group 1 interrupted SP_EL1.
        "mrs x3, sp_el0",
        "lsr x5, x0, #2",
        "cmp x5, #1",
        "b.ne 4f",
        "mov x3, sp",
        "4: mrs x1, elr_el1",
        "mov x2, xzr",
        "mov x4, xzr",
        // IRQ and FIQ do not update ESR. FAR is meaningful for selected aborts.
        "and x5, x0, #3",
        "cmp x5, #1",
        "b.eq 5f",
        "cmp x5, #2",
        "b.eq 5f",
        "mrs x4, esr_el1",
        "lsr x5, x4, #26",
        "orr x5, x5, #1",
        "cmp x5, #0x21",
        "b.eq 7f",
        "cmp x5, #0x25",
        "b.ne 5f",
        "7: tbnz x4, #10, 5f",
        "mrs x2, far_el1",
        "5: adrp x5, {stack}",
        "add x5, x5, :lo12:{stack}",
        "add sp, x5, {bytes}",
        "b {report}",
        "6: wfe",
        "b 6b",
        available = sym diagnostic::AVAILABLE,
        stack = sym diagnostic::STACK,
        bytes = const diagnostic::BYTES,
        report = sym diagnostic::report,
    );
}
