use crate::metal::{
    platform::boot::handoff::{Descriptions, Facts, uefi},
    platform::boot::trap as diagnostic,
    processor::hart::{local, x86_64},
};
mod major;
pub(crate) use major::major as enter_major;

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

use x86_64::Descriptor;

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

/// Admit the loader facts and check the actual local APIC against MADT.
pub(crate) unsafe fn admit<'a>(
    hart: usize,
    input: *const (),
    descriptions: &mut Descriptions<'a>,
) -> Option<Facts<'a>> {
    let facts = unsafe { uefi::admit(hart, x86_64::boot_identity(), input, descriptions) }?;
    if let Some(acpi) = descriptions.acpi.as_ref()
        && !x86_64::matches_madt(hart, acpi.madt()?)
    {
        return None;
    }
    Some(facts)
}

pub(crate) fn established() -> bool {
    let (flags, cr0): (usize, usize);
    unsafe {
        asm!("pushfq", "pop {}", "mov {}, cr0", out(reg) flags, out(reg) cr0);
    }
    cr0 & (1 << 31) != 0 && flags & (1 << 9) == 0 && entry().is_some()
}

pub(in crate::metal) fn entry() -> Option<local::Entry> {
    local::Entry::admit(
        x86_64::Entry {
            base: (&raw const IDT).addr(),
            limit: (size_of::<[Gate; 256]>() - 1) as u16,
        },
        diagnostic::stack_range(),
    )
}

pub(crate) fn park() -> ! {
    stop()
}

mod minor;
pub use minor::Startup;
pub(crate) use minor::{MinorContext, minor_context, wake_minor};

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
        "xchg [rip + {top}], rax",
        "test rax, rax",
        "jz 4f",
        "mov rdi, rsp",
        "mov rsp, rax",
        "call {capture}",
        "4: hlt",
        "jmp 4b",
        stride = const VECTOR_BYTES,
        top = sym diagnostic::TOP,
        capture = sym capture,
    );
}

// The vector stubs normalize the error-code slot. Hardware supplies the rest
// of the 64-bit frame, including the interrupted RSP even for a same-ring trap.
#[repr(C)]
struct Frame {
    vector: usize,
    error: usize,
    rip: usize,
    cs: usize,
    flags: usize,
    rsp: usize,
    ss: usize,
}

unsafe extern "sysv64" fn capture(frame: *const Frame) -> ! {
    let Frame {
        vector,
        error,
        rip,
        rsp,
        ..
    } = unsafe { frame.read() };
    let mut value = 0;
    if vector == 14 {
        unsafe { asm!("mov {}, cr2", out(reg) value, options(nomem, nostack, preserves_flags)) };
    }
    diagnostic::Trap {
        cause: vector,
        pc: rip,
        value,
        stack: rsp,
        status: error,
    }
    .report()
}
