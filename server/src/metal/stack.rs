//! Image-owned, exclusively reserved storage for the primary boot CPU.
use core::ops::Range;

#[cfg(target_arch = "riscv64")]
const BYTES: usize = 16 * 1024;
#[cfg(not(target_arch = "riscv64"))]
const BYTES: usize = 32 * 1024;

#[repr(C, align(16))]
pub(super) struct Stack([u8; BYTES]);

// RV64 clears BSS after selecting this stack. Its linker places the reservation
// outside the cleared interval and preserves the firmware image's linker symbols.
#[cfg_attr(target_arch = "riscv64", unsafe(link_section = ".limen.boot.stack"))]
pub(super) static mut STACK: Stack = Stack([0; BYTES]);

/// Ordinary stack reservation of the primary boot CPU.
pub fn stack_range() -> Range<usize> {
    let start = unsafe { (&raw const STACK.0).addr() };
    start..start + size_of::<Stack>()
}

/// Read the hardware SP at the admission check. Taking the address of a Rust
/// local would leave the result dependent on how the compiler places it.
pub(crate) fn active() -> bool {
    let sp: usize;
    unsafe {
        #[cfg(target_arch = "riscv64")]
        core::arch::asm!("mv {}, sp", out(reg) sp, options(nomem, nostack));
        #[cfg(target_arch = "aarch64")]
        core::arch::asm!("mov {}, sp", out(reg) sp, options(nomem, nostack, preserves_flags));
        #[cfg(target_arch = "x86_64")]
        core::arch::asm!("mov {}, rsp", out(reg) sp, options(nomem, nostack, preserves_flags));
    }
    stack_range().contains(&sp)
}
