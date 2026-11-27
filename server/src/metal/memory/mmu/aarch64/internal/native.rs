use super::{Controls, Format, Root};
use crate::metal::memory::mmu::SwitchError;
use crate::metal::processor::hart::local::try_current_hart;
use core::{arch::asm, marker::PhantomData};
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

/// Qualified exclusive access to one EL1 stage-one root on the selected PE.
/// The other root remains directly owned by the calling platform.
#[must_use]
pub struct Translation {
    hart: usize,
    format: Format,
    original: Controls,
    approved: Controls,
    pending: Option<Controls>,
    local: PhantomData<*mut ()>,
}

impl Translation {
    /// Admit the actual EL1 translation configuration and selected root.
    ///
    /// # Safety
    /// The caller controls stage one exclusively for this operation, in
    /// non-secure EL1 with no active stage two or extended descriptor
    /// interpretation. Keep this authority on the same PE and retain both
    /// roots and their real mappings while any effect is uncertain.
    pub unsafe fn admit(hart: usize, selected: Root) -> Result<Self, SwitchError> {
        Self::check(hart)?;
        let original = unsafe { Controls::current() };
        let format = unsafe { original.local_format(selected) }.ok_or(SwitchError::Scope)?;
        let root = match selected {
            Root::Lower => original.ttbr0,
            Root::Upper => original.ttbr1,
        };
        if original.ttbr0 >> 48 != 0
            || !root.is_multiple_of(4096)
            || root >= 1 << format.physical_bits
        {
            return Err(SwitchError::Scope);
        }
        Ok(Self {
            hart,
            format,
            original,
            approved: original,
            pending: None,
            local: PhantomData,
        })
    }

    pub fn format(&self) -> Format {
        self.format
    }

    pub fn original(&self) -> Controls {
        self.original
    }

    pub fn current(&self) -> Option<Controls> {
        self.pending.is_none().then_some(self.approved)
    }

    /// The last verified controls and the proposed controls of an uncertain write.
    pub fn pending(&self) -> Option<(Controls, Controls)> {
        self.pending.map(|next| (self.approved, next))
    }

    fn check(hart: usize) -> Result<(), SwitchError> {
        if !crate::metal::processor::hart::aarch64::native::masked_stack()
            || try_current_hart().is_none_or(|here| here.get() != hart)
        {
            return Err(SwitchError::Context);
        }
        Ok(())
    }

    /// Switch the selected ASID-zero TTBR and complete conservative stage-one
    /// maintenance in the participating Inner Shareable domain.
    ///
    /// # Safety
    /// All code, stack, handlers and data needed by this operation must use
    /// the independently retained opposite root. Stop old-root accesses
    /// and exclude conflicting ASID use or re-entry on every PE until
    /// maintenance completes. The next tables must be live and coherent.
    /// Separate remote root-exit and memory reclamation evidence is required.
    pub unsafe fn switch(
        &mut self,
        current: Controls,
        root: PhysicalAddress,
    ) -> Result<(), SwitchError> {
        Self::check(self.hart)?;
        let previous = self.current().ok_or(SwitchError::Unconfirmed)?;
        if !root.get().is_multiple_of(4096) || root.get() >= 1 << self.format.physical_bits {
            return Err(SwitchError::Scope);
        }
        if previous != current || unsafe { Controls::current() } != previous {
            return Err(SwitchError::Current);
        }
        let next = match self.format.root {
            Root::Lower => Controls {
                ttbr0: root.get(),
                ..previous
            },
            Root::Upper => Controls {
                ttbr1: root.get(),
                ..previous
            },
        };
        self.pending = Some(next);
        unsafe { self.install(next) }
    }

    /// Repeat the retained switch and its native maintenance on the same PE.
    ///
    /// # Safety
    /// Preserve the original switch's ownership, mappings and ordering
    /// requirements for both recorded roots. No other effect may have used
    /// this authority since the uncertain write.
    pub unsafe fn retry(&mut self) -> Result<(), SwitchError> {
        Self::check(self.hart)?;
        let next = self.pending.ok_or(SwitchError::Current)?;
        let observed = unsafe { Controls::current() };
        if observed != self.approved && observed != next {
            return Err(SwitchError::Unconfirmed);
        }
        unsafe { self.install(next) }
    }

    unsafe fn install(&mut self, next: Controls) -> Result<(), SwitchError> {
        let disable_walk = match self.format.root {
            Root::Lower => 1 << 7,
            Root::Upper => 1 << 23,
        };
        let disabled = self.approved.tcr | disable_walk;
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
        if unsafe { Controls::current() } != next {
            return Err(SwitchError::Unconfirmed);
        }
        self.approved = next;
        self.pending = None;
        Ok(())
    }
}
