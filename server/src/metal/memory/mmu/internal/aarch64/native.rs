use super::super::SwitchError;
use super::{Controls, Format, Root};
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
    pub unsafe fn local_format(self, root: Root) -> Option<Format> {
        let features;
        unsafe {
            asm!("mrs {}, ID_AA64MMFR0_EL1", out(reg) features, options(nostack, nomem, preserves_flags));
        }
        self.format(root, features)
    }

    /// Switch one local ASID-zero TTBR and complete conservative stage-one
    /// translation maintenance in the participating Inner Shareable domain.
    ///
    /// # Safety
    /// The caller owns masked, nonmigrating non-secure EL1 execution and the
    /// stage-one configuration, excludes stage two and extended descriptor
    /// controls, and ensures all code, stack, handlers and data needed here
    /// use the independently retained opposite root. Stop all accesses through the old
    /// root and exclude conflicting ASID use or re-entry on every PE until
    /// maintenance completes. All new tables must be live and coherent.
    /// This produces no remote root-exit or memory reclamation receipt.
    pub unsafe fn switch(
        self,
        hart: usize,
        selected: Root,
        root: PhysicalAddress,
    ) -> Result<(), SwitchError> {
        if !crate::metal::processor::hart::aarch64::native::masked_stack()
            || try_current_hart().is_none_or(|here| here.get() != hart)
        {
            return Err(SwitchError::Context);
        }
        let Some(format) = (unsafe { self.local_format(selected) }) else {
            return Err(SwitchError::Scope);
        };
        let original = match selected {
            Root::Lower => self.ttbr0,
            Root::Upper => self.ttbr1,
        };
        if self.ttbr0 >> 48 != 0
            || original >> 48 != 0
            || original & 0xfff != 0
            || !root.get().is_multiple_of(4096)
            || root.get() >= 1 << format.physical_bits
        {
            return Err(SwitchError::Scope);
        }
        if unsafe { Self::current() } != self {
            return Err(SwitchError::Current);
        }
        let disabled = self.tcr
            | match selected {
                Root::Lower => 1 << 7,
                Root::Upper => 1 << 23,
            };
        unsafe {
            asm!("msr tcr_el1, {}", "isb", "dsb ishst", "tlbi vmalle1is", "dsb ish", "isb",
                in(reg) disabled, options(nostack, preserves_flags));
            match selected {
                Root::Lower => {
                    asm!("msr ttbr0_el1, {}", in(reg) root.get(), options(nostack, preserves_flags))
                }
                Root::Upper => {
                    asm!("msr ttbr1_el1, {}", in(reg) root.get(), options(nostack, preserves_flags))
                }
            }
            asm!("isb", "msr tcr_el1, {}", "isb", in(reg) self.tcr, options(nostack, preserves_flags));
        }
        let now = unsafe { Self::current() };
        let installed = match selected {
            Root::Lower => now.ttbr0,
            Root::Upper => now.ttbr1,
        };
        (now.tcr == self.tcr && installed == root.get())
            .then_some(())
            .ok_or(SwitchError::Unconfirmed)
    }
}
