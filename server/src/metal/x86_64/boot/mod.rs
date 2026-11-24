use crate::metal::generic::boot::trap as diagnostic;
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

pub(crate) fn stack_pointer() -> usize {
    let sp: usize;
    unsafe { asm!("mov {}, rsp", out(reg) sp, options(nomem, nostack)) };
    sp
}

pub type DeliveryError = isize;

// A 64-bit interrupt gate saves RIP, CS, RFLAGS, RSP and SS. Normalize the
// hardware error-code slot before entering the common register-only stub.
const VECTOR_BYTES: usize = 16;
#[unsafe(naked)]
unsafe extern "sysv64" fn vectors() -> ! {
    naked_asm!(
        ".set vector, 0",
        ".rept 256",
        "2:",
        ".if vector != 8 && vector != 10 && vector != 11 && vector != 12 && vector != 13 && vector != 14 && vector != 17 && vector != 21 && vector != 29 && vector != 30",
        "push 0",
        ".endif",
        "push vector",
        "jmp 3f",
        ".fill {stride} - (. - 2b), 1, 0x90",
        ".set vector, vector + 1",
        ".endr",
        "3: cli",
        "cld",
        "xor eax, eax",
        "xchg [rip + {available}], eax",
        "test eax, eax",
        "jz 5f",
        "mov rdi, [rsp]",       // vector
        "mov r8, [rsp + 8]",    // hardware error code or zero
        "mov rsi, [rsp + 16]",  // RIP
        "mov rcx, [rsp + 40]",  // interrupted RSP
        "xor edx, edx",
        "cmp edi, 14",
        "jne 4f",
        "mov rdx, cr2",
        "4: lea rsp, [rip + {stack} + {bytes}]",
        "call {report}",
        "5: hlt",
        "jmp 5b",
        stride = const VECTOR_BYTES,
        available = sym diagnostic::AVAILABLE,
        stack = sym diagnostic::STACK,
        bytes = const diagnostic::BYTES,
        report = sym report,
    );
}

// The assembly stub uses SysV registers even for a UEFI image.
extern "sysv64" fn report(vector: usize, pc: usize, value: usize, sp: usize, error: usize) -> ! {
    diagnostic::report(vector, pc, value, sp, error)
}
