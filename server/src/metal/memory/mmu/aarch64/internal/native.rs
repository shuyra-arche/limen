use super::{Controls, Format, Root};
use crate::metal::memory::mmu::{LocalContext, SwitchError};
use crate::metal::processor::hart::local::try_current_hart;
use core::arch::asm;
use limen_treaty::model::PhysicalAddress;

impl Controls {
    /// # Safety
    /// Execute on the intended PE with EL1 translation system register access.
    pub unsafe fn current() -> Self {
        let (mair, tcr, ttbr0, ttbr1, sctlr);
        unsafe {
            asm!("mrs {}, mair_el1", "mrs {}, tcr_el1", "mrs {}, ttbr0_el1", "mrs {}, ttbr1_el1",
                "mrs {}, sctlr_el1", out(reg) mair, out(reg) tcr, out(reg) ttbr0, out(reg) ttbr1,
                out(reg) sctlr, options(nostack, nomem, preserves_flags));
        }
        Self {
            mair,
            tcr,
            ttbr0,
            ttbr1,
            sctlr,
        }
    }

    /// # Safety
    /// Execute on the PE described by this snapshot with MMFR0 access.
    unsafe fn local_format(self, root: Root) -> Option<Format> {
        let features;
        unsafe {
            asm!("mrs {}, ID_AA64MMFR0_EL1", out(reg) features, options(nostack, nomem, preserves_flags));
        }
        self.format(root, features)
    }
}

/// Bind this PE's selected EL1 stage-one root installation authority.
///
/// # Safety
/// The caller owns stage one in non-secure EL1, without active stage two
/// or extended descriptor interpretation. Keep that ownership, the
/// independently owned opposite root, and all affected table storage
/// live through actual retirement.
pub unsafe fn admit_root(
    hart: usize,
    selected: Root,
) -> Result<(LocalContext<'static>, Format), SwitchError> {
    NativeRoot::check(hart)?;
    let initial = unsafe { Controls::current() };
    let format = unsafe { initial.local_format(selected) }.ok_or(SwitchError::Scope)?;
    let native = NativeRoot { format };
    let root = native.root(initial);
    if initial.ttbr0 >> 48 != 0 || native.with_root(initial, root).is_none() {
        return Err(SwitchError::Scope);
    }
    Ok((LocalContext::bind(hart, native, initial), format))
}

pub(in crate::metal::memory::mmu) struct NativeRoot {
    format: Format,
}

impl NativeRoot {
    pub(in crate::metal::memory::mmu) fn check(hart: usize) -> Result<(), SwitchError> {
        if !crate::metal::processor::hart::aarch64::native::masked_stack()
            || try_current_hart().is_none_or(|here| here.get() != hart)
        {
            return Err(SwitchError::Context);
        }
        Ok(())
    }

    pub(in crate::metal::memory::mmu) fn profile(&self) -> Format {
        self.format
    }

    pub(in crate::metal::memory::mmu) unsafe fn observe() -> Controls {
        unsafe { Controls::current() }
    }

    pub(in crate::metal::memory::mmu) fn physical_root(
        &self,
        controls: Controls,
    ) -> Option<PhysicalAddress> {
        Some(self.root(controls))
    }

    pub(in crate::metal::memory::mmu) fn supports(
        &self,
        previous: Controls,
        requested: Controls,
    ) -> bool {
        self.with_root(previous, self.root(requested)) == Some(requested)
    }

    fn root(&self, controls: Controls) -> PhysicalAddress {
        PhysicalAddress::new(match self.format.root {
            Root::Lower => controls.ttbr0,
            Root::Upper => controls.ttbr1,
        })
    }

    pub(in crate::metal::memory::mmu) fn with_root(
        &self,
        controls: Controls,
        root: PhysicalAddress,
    ) -> Option<Controls> {
        (root.get().is_multiple_of(4096) && root.get() < 1 << self.format.physical_bits).then_some(
            match self.format.root {
                Root::Lower => Controls {
                    ttbr0: root.get(),
                    ..controls
                },
                Root::Upper => Controls {
                    ttbr1: root.get(),
                    ..controls
                },
            },
        )
    }

    pub(in crate::metal::memory::mmu) unsafe fn install(&self, next: Controls) {
        let disable_walk = match self.format.root {
            Root::Lower => 1 << 7,
            Root::Upper => 1 << 23,
        };
        let disabled = next.tcr | disable_walk;
        unsafe {
            asm!("msr tcr_el1, {}", "isb", "dsb ishst", "tlbi vmalle1is", "dsb ish", "isb",
                in(reg) disabled, options(nostack, preserves_flags));
            match self.format.root {
                Root::Lower => {
                    asm!("msr ttbr0_el1, {}", in(reg) next.ttbr0, options(nostack, preserves_flags))
                }
                Root::Upper => {
                    asm!("msr ttbr1_el1, {}", in(reg) next.ttbr1, options(nostack, preserves_flags))
                }
            }
            asm!("isb", "msr tcr_el1, {}", "isb", in(reg) next.tcr, options(nostack, preserves_flags));
        }
    }
}
