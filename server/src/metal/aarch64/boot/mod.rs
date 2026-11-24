use crate::metal::generic::stack::Stack;
mod major;
pub(crate) use major::major as enter_major;

use core::arch::asm;

pub(crate) fn established() -> bool {
    let (el, daif, spsel, vector): (usize, usize, usize, usize);
    unsafe {
        asm!("mrs {}, CurrentEL", "mrs {}, DAIF", "mrs {}, SPSel", "mrs {}, VBAR_EL1",
             out(reg) el, out(reg) daif, out(reg) spsel, out(reg) vector,
             options(nostack, nomem, preserves_flags));
    }
    el == 4 && daif & 0x3c0 == 0x3c0 && spsel == 1 && vector == major::vectors as *const () as usize
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
pub(crate) use minor::{MinorContext, current_hart, minor_context, wake_minor};

pub(crate) const STACK_BYTES: usize = 32 * 1024;
pub(in crate::metal) static mut STACK: Stack = Stack::new();

pub(crate) fn stack_pointer() -> usize {
    let sp: usize;
    unsafe { asm!("mov {}, sp", out(reg) sp, options(nomem, nostack)) };
    sp
}

pub type DeliveryError = core::convert::Infallible;
