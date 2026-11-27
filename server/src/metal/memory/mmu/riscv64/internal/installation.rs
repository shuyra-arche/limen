//! Local Hart translation control through the supervisor SATP register.

use core::arch::asm;
use limen_treaty::model::PhysicalAddress;

use super::{
    context::Context,
    description::{MaximumMode, RiscvMmu},
};
use crate::metal::{
    memory::mmu::treaty::{LocalContext, SwitchError},
    processor::{executor::riscv64::native, treaty::try_current_processor},
};

impl Context {
    pub fn current() -> Option<Self> {
        Self::from_bits(read())
    }
}

/// Bind the original Hart's ASID-zero SATP installation authority.
///
/// # Safety
/// The caller controls SATP exclusively on this described Hart and
/// establishes the actual supervisor environment. Preserve this
/// qualification through all switches and old translation obligations.
pub unsafe fn admit_context(description: RiscvMmu<'_>) -> Result<LocalContext<'_>, SwitchError> {
    let hart = usize::try_from(description.hart()).map_err(|_| SwitchError::Scope)?;
    NativeRoot::check(hart)?;
    let native = NativeRoot { description };
    let current = Context::current().ok_or(SwitchError::Current)?;
    if !native.supports(current, current) {
        return Err(SwitchError::Scope);
    }
    Ok(LocalContext::bind(hart, native, current))
}

pub(in crate::metal::memory::mmu) struct NativeRoot<'a> {
    description: RiscvMmu<'a>,
}

impl NativeRoot<'_> {
    pub(in crate::metal::memory::mmu) fn check(hart: usize) -> Result<(), SwitchError> {
        if try_current_processor().is_none_or(|binding| binding.get() != hart)
            || !native::status().masked()
        {
            return Err(SwitchError::Context);
        }
        Ok(())
    }

    pub(in crate::metal::memory::mmu) fn profile(&self) -> MaximumMode {
        self.description.maximum()
    }

    pub(in crate::metal::memory::mmu) unsafe fn observe() -> Context {
        // Preserve actual readback, including unsupported values. Even an
        // unexpected read leaves the original pending operation intact.
        Context(read())
    }

    pub(in crate::metal::memory::mmu) fn physical_root(
        &self,
        context: Context,
    ) -> Option<PhysicalAddress> {
        context.root()
    }

    pub(in crate::metal::memory::mmu) fn supports(
        &self,
        _previous: Context,
        requested: Context,
    ) -> bool {
        requested
            .mode()
            .is_none_or(|mode| self.profile().supports(mode))
    }

    pub(in crate::metal::memory::mmu) unsafe fn install(&self, next: Context) {
        // All supported paged contexts share ASID zero.
        unsafe {
            asm!("sfence.vma x0, x0", "csrw satp, {next}", "sfence.vma x0, x0",
                next = in(reg) next.bits(), options(nostack));
        }
    }
}

fn read() -> u64 {
    let bits;
    unsafe {
        asm!("csrr {}, satp", out(reg) bits, options(nostack, nomem));
    }
    bits
}
