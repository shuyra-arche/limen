//! Local root installation and native translation maintenance.

use super::{context::Controls, format::Format};
use crate::metal::memory::mmu::treaty::{LocalContext, SwitchError};
use crate::metal::processor::treaty::try_current_processor;
use core::arch::asm;
use limen_treaty::model::PhysicalAddress;

impl Controls {
    /// # Safety
    /// Execute with privileged CR and MSR access on the intended processor.
    pub unsafe fn current() -> Self {
        let (cr0, cr3, cr4): (usize, usize, usize);
        let (low, high): (u32, u32);
        unsafe {
            asm!("mov {}, cr0", "mov {}, cr3", "mov {}, cr4", out(reg) cr0, out(reg) cr3, out(reg) cr4, options(nostack, nomem));
            asm!("rdmsr", in("ecx") 0xc000_0080u32, out("eax") low, out("edx") high, options(nostack, nomem));
        }
        let efer = u64::from(high) << 32 | u64::from(low);
        let pat = if core::arch::x86_64::__cpuid(1).edx & (1 << 16) != 0 {
            let (low, high): (u32, u32);
            unsafe {
                asm!("rdmsr", in("ecx") 0x277u32, out("eax") low, out("edx") high,
                    options(nostack, nomem));
            }
            Some(u64::from(high) << 32 | u64::from(low))
        } else {
            None
        };
        Self {
            cr0,
            cr3,
            cr4,
            efer,
            pat,
        }
    }

    /// Check the executing CPU's physical address width and optional 1 GiB pages.
    fn local_format(self) -> Option<Format> {
        use core::arch::x86_64::__cpuid;
        if self.pat.is_none() || __cpuid(0x8000_0000).eax < 0x8000_0008 {
            return None;
        }
        self.format(
            __cpuid(0x8000_0008).eax as u8,
            __cpuid(0x8000_0001).edx & (1 << 26) != 0,
        )
    }
}

/// Bind this processor's four-level, untagged CR3 installation authority.
///
/// # Safety
/// The caller owns the local root registers and PAT exclusively, with
/// physical address modifiers excluded. Keep that ownership, effective
/// memory types and all affected table storage live through retirement.
pub unsafe fn admit_root(processor: usize) -> Result<(LocalContext<'static>, Format), SwitchError> {
    NativeRoot::check(processor)?;
    let initial = unsafe { Controls::current() };
    let format = initial.local_format().ok_or(SwitchError::Scope)?;
    Ok((
        LocalContext::bind(processor, NativeRoot { format }, initial),
        format,
    ))
}

pub(in crate::metal::memory::mmu) struct NativeRoot {
    format: Format,
}

impl NativeRoot {
    pub(in crate::metal::memory::mmu) fn check(processor: usize) -> Result<(), SwitchError> {
        use crate::metal::processor::description::x86_64::status;
        let flags: usize;
        unsafe { asm!("pushfq", "pop {}", out(reg) flags) };
        if !status::interrupts_masked(flags)
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
        PhysicalAddress::new(controls.cr3 as u64)
    }

    pub(in crate::metal::memory::mmu) fn with_root(
        &self,
        controls: Controls,
        root: PhysicalAddress,
    ) -> Option<Controls> {
        (root.get().is_multiple_of(4096) && root.get() < 1 << self.format.physical_bits).then_some(
            Controls {
                cr3: root.get() as usize,
                ..controls
            },
        )
    }

    pub(in crate::metal::memory::mmu) unsafe fn install(&self, next: Controls) {
        unsafe {
            asm!("mov cr3, {next}", next = in(reg) next.cr3, options(nostack, preserves_flags));
        }
    }
}
