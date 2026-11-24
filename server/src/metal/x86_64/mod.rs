use super::stack::{STACK, Stack};
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
    next(input)
}

static mut IDT: core::mem::MaybeUninit<[Gate; 256]> = core::mem::MaybeUninit::uninit();

/// Transfer to the kernel stack and install fail-stop interrupt gates.
///
/// # Safety
/// Call once, after successful ExitBootServices, on the sole boot CPU in
/// 64-bit ring 0. Keep the loader mappings, GDT, image and input reserved.
/// The callback uses the System V ABI even when the loader uses EFIAPI.
#[unsafe(naked)]
pub unsafe extern "sysv64" fn enter(input: usize, next: extern "sysv64" fn(usize) -> !) -> ! {
    naked_asm!(
        "mov ax, cs",
        "test al, 3",
        "jnz 3f",
        "cli",
        "mov rax, cr0",
        "bt rax, 31",
        "jnc 2f",
        "cld",
        "lea rsp, [rip + {stack} + {size}]",
        "call {install}",
        "2: hlt",
        "jmp 2b",
        "3: pause",
        "jmp 3b",
        stack = sym STACK,
        size = const size_of::<Stack>(),
        install = sym install,
    );
}
