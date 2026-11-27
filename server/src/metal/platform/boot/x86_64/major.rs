use crate::metal::platform::boot::treaty::MajorMaterial;
use crate::metal::platform::firmware::uefi::Loader;
use crate::metal::processor::execution::terminal::x86_64 as terminal;
use core::arch::naked_asm;

/// Transfer to the kernel stack and install diagnostic interrupt gates.
///
/// # Safety
/// Call once, after successful ExitBootServices, on the sole boot CPU in
/// 64-bit ring 0. Keep the loader mappings, GDT, image and input reserved.
/// Keep the Loader and its immutable buffers valid across Boot delivery.
#[unsafe(naked)]
pub unsafe extern "sysv64" fn major(
    hart: usize,
    input: &Loader<'_>,
    stack_top: usize,
    material: &'static MajorMaterial,
) -> ! {
    naked_asm!(
        "mov ax, cs",
        "test al, 3",
        "jnz 3f",
        "cli",
        "mov rax, cr0",
        "bt rax, 31",
        "jnc 2f",
        "cld",
        "mov rsp, rdx",
        "call {install}",
        "2: hlt",
        "jmp 2b",
        "3: pause",
        "jmp 3b",
        install = sym install,
    );
}

unsafe extern "sysv64" fn install(
    hart: usize,
    input: *const (),
    stack_top: usize,
    material: &'static MajorMaterial,
) -> ! {
    unsafe { crate::metal::processor::executor::x86_64::native::clear_local() };
    unsafe { terminal::initialize() };
    unsafe { crate::entry::boot::start(hart, input, stack_top, material) }
}
