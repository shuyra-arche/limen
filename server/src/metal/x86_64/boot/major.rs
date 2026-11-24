use super::{Gate, IDT, STACK, load_idt, stop};
use crate::entry::boot::Loader;
use core::arch::{asm, naked_asm};

/// Transfer to the kernel stack and install fail-stop interrupt gates.
///
/// # Safety
/// Call once, after successful ExitBootServices, on the sole boot CPU in
/// 64-bit ring 0. Keep the loader mappings, GDT, image and input reserved.
/// Keep the Loader and its immutable buffers valid across Boot delivery.
#[unsafe(naked)]
pub unsafe extern "sysv64" fn major(hart: usize, input: &Loader<'_>) -> ! {
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
        size = const super::STACK_BYTES,
        install = sym install,
    );
}

unsafe extern "sysv64" fn install(hart: usize, input: *const ()) -> ! {
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
    load_idt();
    unsafe { crate::entry::boot::start(hart, input) }
}
