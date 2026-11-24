use crate::metal::console::Services;

pub(crate) type Continuation = extern "C" fn(usize) -> !;
pub(crate) const REQUIRES_DIAGNOSTICS: bool = false;

pub(crate) fn services() -> Services {
    #[cfg(feature = "qemu-virt")]
    {
        super::board::services()
    }
    #[cfg(not(feature = "qemu-virt"))]
    {
        Services {
            output: None,
            reset: None,
        }
    }
}

use crate::metal::stack::{STACK, Stack};
use core::arch::{asm, global_asm, naked_asm};

global_asm!(
    ".text",
    ".balign 2048",
    ".global limen_vectors",
    "limen_vectors:",
    ".rept 16",
    "b 2f",
    ".balign 128",
    ".endr",
    "2: msr daifset, #15",
    "3: wfe",
    "b 3b",
);

/// Enter the diagnostic callback on the boot stack with masked exceptions
/// and a fail-stop vector table. Preserve the loader's input word.
///
/// # Safety
/// Call once on the boot PE, in non-secure EL1 with the image addressable.
/// Other PEs must stay parked. The callback and its input must remain valid.
#[unsafe(naked)]
pub unsafe extern "C" fn enter(input: usize, next: extern "C" fn(usize) -> !) -> ! {
    naked_asm!(
        "mrs x2, CurrentEL",
        "cmp x2, #4",
        "b.ne 2f",
        "msr daifset, #15",
        "msr spsel, #1",
        "adrp x2, {stack}",
        "add x2, x2, :lo12:{stack}",
        "add sp, x2, {size}",
        "adrp x2, limen_vectors",
        "add x2, x2, :lo12:limen_vectors",
        "msr vbar_el1, x2",
        "isb",
        "b {loaded}",
        "2: wfe",
        "b 2b",
        loaded = sym crate::entry::boot::loaded,
        stack = sym STACK,
        size = const size_of::<Stack>(),
    );
}

pub(crate) fn established() -> bool {
    unsafe extern "C" {
        fn limen_vectors();
    }
    let (el, daif, spsel, vector): (usize, usize, usize, usize);
    unsafe {
        asm!("mrs {}, CurrentEL", "mrs {}, DAIF", "mrs {}, SPSel", "mrs {}, VBAR_EL1",
             out(reg) el, out(reg) daif, out(reg) spsel, out(reg) vector,
             options(nostack, nomem, preserves_flags));
    }
    el == 4 && daif & 0x3c0 == 0x3c0 && spsel == 1 && vector == limen_vectors as *const () as usize
}
pub(crate) fn park() -> ! {
    loop {
        unsafe { asm!("wfe", options(nomem, nostack)) };
    }
}
