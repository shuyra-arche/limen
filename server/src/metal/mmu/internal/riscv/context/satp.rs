//! Local Hart translation control through the supervisor SATP register.

use core::arch::asm;

use super::Context;

impl Context {
    pub fn current() -> Option<Self> {
        Self::from_bits(read())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SwitchError {
    Current,
    Rejected,
}

/// Install a context on this Hart and invalidate its local translations.
///
/// # Safety
///
/// The caller owns local translation control with interrupts excluded.
/// The new context must preserve the complete live code, stack, trap and data
/// paths, with its page-table stores visible on this Hart. The caller separately
/// owns instruction visibility, remote synchronization and old-root retirement.
pub unsafe fn switch(current: Context, next: Context) -> Result<(), SwitchError> {
    if read() != current.bits() {
        return Err(SwitchError::Current);
    }
    // Full local fences are required here because contexts share ASID zero.
    let installed: u64;
    unsafe {
        asm!(
            "sfence.vma x0, x0",
            "csrw satp, {next}",
            "sfence.vma x0, x0",
            "csrr {installed}, satp",
            next = in(reg) next.bits(),
            installed = lateout(reg) installed,
            options(nostack),
        );
    }
    if installed == next.bits() {
        Ok(())
    } else {
        Err(SwitchError::Rejected)
    }
}

fn read() -> u64 {
    let bits;
    unsafe {
        asm!("csrr {}, satp", out(reg) bits, options(nostack, nomem));
    }
    bits
}
