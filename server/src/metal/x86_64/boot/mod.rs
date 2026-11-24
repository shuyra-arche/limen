use crate::metal::generic::stack::Stack;
mod major;
pub(crate) use major::major as enter_major;

use core::arch::{asm, naked_asm, x86_64::__cpuid};

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct Gate {
    low: u16,
    selector: u16,
    ist: u8,
    attributes: u8,
    middle: u16,
    high: u32,
    reserved: u32,
}

#[repr(C, packed)]
struct Descriptor {
    limit: u16,
    base: usize,
}

#[unsafe(naked)]
extern "sysv64" fn stop() -> ! {
    naked_asm!("cli", "2: hlt", "jmp 2b");
}

fn load_idt() {
    let idtr = Descriptor {
        limit: (size_of::<[Gate; 256]>() - 1) as u16,
        base: (&raw const IDT).addr(),
    };
    unsafe { asm!("lidt [{}]", in(reg) &idtr, options(nostack, preserves_flags)) };
}

static mut IDT: core::mem::MaybeUninit<[Gate; 256]> = core::mem::MaybeUninit::uninit();

pub(crate) fn current_hart() -> usize {
    (__cpuid(1).ebx >> 24) as usize
}

pub(crate) fn established() -> bool {
    let mut idtr = Descriptor { limit: 0, base: 0 };
    let (flags, cr0): (usize, usize);
    let cs: u16;
    unsafe {
        asm!("sidt [{}]", in(reg) &mut idtr, options(nostack, preserves_flags));
        asm!("pushfq", "pop {}", "mov {}, cr0", "mov {:x}, cs",
             out(reg) flags, out(reg) cr0, out(reg) cs);
    }
    cs & 3 == 0
        && cr0 & (1 << 31) != 0
        && flags & (1 << 9) == 0
        && idtr.limit as usize + 1 == size_of::<[Gate; 256]>()
        && idtr.base == (&raw const IDT).addr()
}

pub(crate) fn park() -> ! {
    stop()
}

mod minor;
pub(crate) use crate::metal::generic::firmware::uefi as firmware;
pub(crate) mod interface {
    pub use super::firmware::{Loader, major};
    pub use super::minor::Startup;
}
pub(crate) use minor::{MinorContext, minor_context, wake_minor};

pub(crate) const STACK_BYTES: usize = 32 * 1024;
pub(in crate::metal) static mut STACK: Stack<STACK_BYTES> = Stack::new();

pub(crate) fn stack_pointer() -> usize {
    let sp: usize;
    unsafe { asm!("mov {}, rsp", out(reg) sp, options(nomem, nostack)) };
    sp
}

pub type DeliveryError = isize;
