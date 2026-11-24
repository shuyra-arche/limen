mod start;
pub use start::major;

pub(crate) const REQUIRES_DIAGNOSTICS: bool = false;

use core::arch::asm;

pub(crate) fn established() -> bool {
    let (el, daif, spsel, vector): (usize, usize, usize, usize);
    unsafe {
        asm!("mrs {}, CurrentEL", "mrs {}, DAIF", "mrs {}, SPSel", "mrs {}, VBAR_EL1",
             out(reg) el, out(reg) daif, out(reg) spsel, out(reg) vector,
             options(nostack, nomem, preserves_flags));
    }
    el == 4 && daif & 0x3c0 == 0x3c0 && spsel == 1 && vector == start::vectors as *const () as usize
}
pub(crate) fn park() -> ! {
    loop {
        unsafe { asm!("wfe", options(nomem, nostack)) };
    }
}

#[cfg(feature = "qemu-virt")]
mod minor;
#[cfg(feature = "qemu-virt")]
pub(crate) use minor::{MINOR_CONTEXT, MinorContext, current_hart, wake_minor};
