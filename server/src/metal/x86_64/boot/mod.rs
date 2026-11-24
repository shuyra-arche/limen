mod start;
pub use start::enter;

use crate::metal::console::Services;

pub(crate) type Continuation = extern "sysv64" fn(usize) -> !;
pub(crate) const REQUIRES_DIAGNOSTICS: bool = false;

pub(crate) fn services() -> Services {
    #[cfg(feature = "qemu-q35")]
    {
        super::board::services()
    }
    #[cfg(not(feature = "qemu-q35"))]
    {
        Services {
            output: None,
            reset: None,
        }
    }
}

use core::arch::{asm, naked_asm};

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

unsafe extern "sysv64" fn install(input: usize, next: extern "sysv64" fn(usize) -> !) -> ! {
    let selector: u16;
    unsafe { asm!("mov {:x}, cs", out(reg) selector, options(nomem, nostack, preserves_flags)) };
    let addr = stop as *const () as usize;
    let gate = Gate {
        low: addr as u16,
        selector,
        ist: 0,
        attributes: 0x8e,
        middle: (addr >> 16) as u16,
        high: (addr >> 32) as u32,
        reserved: 0,
    };
    // The CPU retains this table after LIDT. Its storage belongs to the image.
    let slots = (&raw mut IDT).cast::<Gate>();
    for index in 0..256 {
        unsafe { slots.add(index).write(gate) };
    }
    let idtr = Descriptor {
        limit: (size_of::<[Gate; 256]>() - 1) as u16,
        base: slots as usize,
    };
    unsafe { asm!("lidt [{}]", in(reg) &idtr, options(nostack, preserves_flags)) };
    crate::entry::boot::loaded(input, next)
}

static mut IDT: core::mem::MaybeUninit<[Gate; 256]> = core::mem::MaybeUninit::uninit();

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
