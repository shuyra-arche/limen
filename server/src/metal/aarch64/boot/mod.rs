mod start;
pub use start::enter;

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
