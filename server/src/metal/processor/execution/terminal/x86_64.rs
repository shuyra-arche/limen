use super::trap as diagnostic;
use crate::metal::processor::executor::x86_64::native::Descriptor;
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

pub(in crate::metal) fn idt() -> Descriptor {
    Descriptor {
        limit: (size_of::<[Gate; 256]>() - 1) as u16,
        base: (&raw const IDT).addr(),
    }
}

pub(in crate::metal) fn load_idt() {
    let idtr = idt();
    unsafe { asm!("lidt [{}]", in(reg) &idtr, options(nostack, preserves_flags)) };
}

static mut IDT: core::mem::MaybeUninit<[Gate; 256]> = core::mem::MaybeUninit::uninit();

/// Initialize the terminal gates once, on the major CPU before any launch.
///
/// # Safety
/// No CPU may use the diagnostic IDT until its initialization completes.
/// The current code selector and gates must remain usable by all entries.
pub(in crate::metal) unsafe fn initialize() {
    let selector: u16;
    unsafe { asm!("mov {:x}, cs", out(reg) selector, options(nomem, nostack, preserves_flags)) };
    let slots = (&raw mut IDT).cast::<Gate>();
    for index in 0..256 {
        let addr = vector_address(index as u8);
        let gate = Gate {
            low: addr as u16,
            selector,
            ist: 0,
            attributes: 0x8e,
            middle: (addr >> 16) as u16,
            high: (addr >> 32) as u32,
            reserved: 0,
        };
        unsafe { slots.add(index).write(gate) };
    }
    load_idt();
}

// A 64-bit interrupt gate saves RIP, CS, RFLAGS, RSP and SS. Normalize the
// hardware error-code slot before entering the common register-only stub.
const VECTOR_BYTES: usize = 16;

pub(in crate::metal::processor) fn vector_address(vector: u8) -> usize {
    vectors as *const () as usize + usize::from(vector) * VECTOR_BYTES
}

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
