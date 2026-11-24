use super::{Continuation, install};
use crate::metal::stack::{STACK, Stack};
use core::arch::naked_asm;

/// Transfer to the kernel stack and install fail-stop interrupt gates.
///
/// # Safety
/// Call once, after successful ExitBootServices, on the sole boot CPU in
/// 64-bit ring 0. Keep the loader mappings, GDT, image and input reserved.
/// The callback uses the System V ABI even when the loader uses EFIAPI.
#[unsafe(naked)]
pub unsafe extern "sysv64" fn enter(input: usize, next: Continuation) -> ! {
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
