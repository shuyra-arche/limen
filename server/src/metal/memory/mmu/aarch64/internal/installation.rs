//! Local root installation and native translation maintenance.

use super::{
    context::Controls,
    format::{Format, Root},
};
use crate::metal::memory::mmu::treaty::{LocalContext, SwitchError};
use crate::metal::processor::treaty::try_current_processor;
use core::arch::asm;
use limen_treaty::model::PhysicalAddress;

const LOWER_WALK_DISABLED: u64 = 1 << 7;
const UPPER_WALK_DISABLED: u64 = 1 << 23;
// The upper root may configure only T1SZ, EPD1, walk cache/shareability
// and TG1. In particular ASID selection and all lower controls stay exact.
const UPPER_WALK_FIELDS: u64 = 0xffbf << 16;
const UPPER_48BIT_WALK: u64 = 0xb510 << 16;

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
    processor: usize,
    selected: Root,
) -> Result<(LocalContext<'static>, Format), SwitchError> {
    NativeRoot::check(processor)?;
    let initial = unsafe { Controls::current() };
    let format = unsafe { initial.local_format(selected) }.ok_or(SwitchError::Scope)?;
    let native = NativeRoot {
        format,
        upper: None,
    };
    let root = native.root(initial);
    if initial.ttbr0 >> 48 != 0 || native.with_root(initial, root).is_none() {
        return Err(SwitchError::Scope);
    }
    Ok((LocalContext::bind(processor, native, initial), format))
}

/// Admit the original disabled and empty upper walk without changing it.
///
/// # Safety
/// The caller owns non-secure EL1 stage one, with no active stage two or
/// extended descriptor interpretation. The independently retained lower
/// walk covers every live code, stack, handler and data access. No other
/// PE or live upper user depends on the disabled configuration, including
/// old cached translations. Keep this scope and the lower walk live
/// through installation and original hardware settlement.
pub unsafe fn admit_inactive_upper(
    processor: usize,
) -> Result<(LocalContext<'static>, Format), SwitchError> {
    NativeRoot::check(processor)?;
    let original = unsafe { Controls::current() };
    if original.ttbr1 != 0
        || original.tcr & UPPER_WALK_DISABLED == 0
        || original.tcr & LOWER_WALK_DISABLED != 0
        || original.ttbr0 >> 48 != 0
    {
        return Err(SwitchError::Scope);
    }
    let enabled = (original.tcr & !UPPER_WALK_FIELDS) | UPPER_48BIT_WALK;
    let candidate = Controls {
        tcr: enabled,
        ..original
    };
    let format = unsafe { candidate.local_format(Root::Upper) }.ok_or(SwitchError::Scope)?;
    let native = NativeRoot {
        format,
        upper: Some(InactiveUpper { original, enabled }),
    };
    Ok((LocalContext::bind(processor, native, original), format))
}

struct InactiveUpper {
    original: Controls,
    enabled: u64,
}

pub(in crate::metal::memory::mmu) struct NativeRoot {
    format: Format,
    upper: Option<InactiveUpper>,
}

impl NativeRoot {
    pub(in crate::metal::memory::mmu) fn check(processor: usize) -> Result<(), SwitchError> {
        if !crate::metal::processor::executor::aarch64::native::masked_stack()
            || try_current_processor().is_none_or(|here| here.get() != processor)
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
        if self
            .upper
            .as_ref()
            .is_some_and(|upper| controls == upper.original)
        {
            None
        } else {
            Some(self.root(controls))
        }
    }

    pub(in crate::metal::memory::mmu) fn supports(
        &self,
        previous: Controls,
        requested: Controls,
    ) -> bool {
        if let Some(upper) = &self.upper {
            if requested == upper.original {
                return true;
            }
        }
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
        if !root.get().is_multiple_of(4096) || root.get() >= 1 << self.format.physical_bits {
            return None;
        }
        if let Some(upper) = &self.upper {
            return (root.get() != 0).then_some(Controls {
                ttbr1: root.get(),
                tcr: upper.enabled,
                ..upper.original
            });
        }
        Some(match self.format.root {
            Root::Lower => Controls {
                ttbr0: root.get(),
                ..controls
            },
            Root::Upper => Controls {
                ttbr1: root.get(),
                ..controls
            },
        })
    }

    #[cfg(limen_cooperative)]
    pub(in crate::metal::memory::mmu) unsafe fn order_table() {
        unsafe { asm!("dsb ishst", options(nostack, preserves_flags)) };
    }

    #[cfg(limen_cooperative)]
    pub(in crate::metal::memory::mmu) unsafe fn synchronize_tree() {
        unsafe {
            asm!(
                "dsb ishst",
                "tlbi vmalle1is",
                "dsb ish",
                "isb",
                options(nostack, preserves_flags)
            );
        }
    }

    #[cfg(limen_cooperative)]
    pub(in crate::metal::memory::mmu) unsafe fn synchronize_leaf(
        address: limen_treaty::model::InternalAddress,
    ) {
        let operand = (address.get() >> 12) & ((1u64 << 44) - 1);
        unsafe {
            asm!("dsb ish", "tlbi vaae1is, {operand}", "dsb ish", "isb",
                operand = in(reg) operand, options(nostack, preserves_flags));
        }
    }

    pub(in crate::metal::memory::mmu) unsafe fn install(&self, next: Controls) {
        let disable_walk = match self.format.root {
            Root::Lower => LOWER_WALK_DISABLED,
            Root::Upper => UPPER_WALK_DISABLED,
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
