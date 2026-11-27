use super::{Controls, Format};
use crate::metal::memory::mmu::SwitchError;
use crate::metal::processor::hart::local::try_current_hart;
use core::{arch::asm, marker::PhantomData};
use limen_treaty::model::PhysicalAddress;

impl Controls {
    /// # Safety
    /// Execute with privileged CR and EFER access on the intended processor.
    pub unsafe fn current() -> Self {
        let (cr0, cr3, cr4): (usize, usize, usize);
        let (low, high): (u32, u32);
        unsafe {
            asm!("mov {}, cr0", "mov {}, cr3", "mov {}, cr4", out(reg) cr0, out(reg) cr3, out(reg) cr4, options(nostack, nomem));
            asm!("rdmsr", in("ecx") 0xc000_0080u32, out("eax") low, out("edx") high, options(nostack, nomem));
        }
        Self {
            cr0,
            cr3,
            cr4,
            efer: u64::from(high) << 32 | u64::from(low),
        }
    }

    /// Check the executing CPU's physical address width and optional 1 GiB pages.
    fn local_format(self) -> Option<Format> {
        use core::arch::x86_64::__cpuid;
        if __cpuid(1).edx & (1 << 16) == 0 || __cpuid(0x8000_0000).eax < 0x8000_0008 {
            return None;
        }
        self.format(
            __cpuid(0x8000_0008).eax as u8,
            __cpuid(0x8000_0001).edx & (1 << 26) != 0,
        )
    }
}

/// Qualified exclusive access to the selected processor's four-level CR3.
/// The control snapshots identify roots, while their actual storage stays
/// with the mapping owner.
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
    /// Admit the actual translation profile on the intended Hart.
    ///
    /// # Safety
    /// The caller controls the local translation registers exclusively and
    /// keeps the active and any candidate roots live through their actual
    /// retirement. Exclude physical address modifiers. Keep this authority
    /// on the same processor and retain it while any effect is uncertain.
    pub unsafe fn admit(hart: usize) -> Result<Self, SwitchError> {
        Self::check(hart)?;
        let original = unsafe { Controls::current() };
        let format = original.local_format().ok_or(SwitchError::Scope)?;
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
        use crate::metal::processor::hart::x86_64::status;
        let flags: usize;
        unsafe { asm!("pushfq", "pop {}", out(reg) flags) };
        if !status::interrupts_masked(flags)
            || try_current_hart().is_none_or(|here| here.get() != hart)
        {
            return Err(SwitchError::Context);
        }
        Ok(())
    }

    /// Install a plain WB root and invalidate this processor's non-global
    /// translations. Success covers no other processor or outstanding access.
    ///
    /// # Safety
    /// Supply a live and visible four-level root preserving every live
    /// code, data, stack and exception path. Retain the old tables and
    /// their remaining obligations.
    pub unsafe fn switch(
        &mut self,
        current: Controls,
        root: PhysicalAddress,
    ) -> Result<(), SwitchError> {
        Self::check(self.hart)?;
        let previous = self.current().ok_or(SwitchError::Unconfirmed)?;
        if !root.get().is_multiple_of(4096) || root.get() >= (1 << self.format.physical_bits) {
            return Err(SwitchError::Scope);
        }
        if previous != current || unsafe { Controls::current() } != previous {
            return Err(SwitchError::Current);
        }
        let next = Controls {
            cr3: root.get() as usize,
            ..previous
        };
        self.pending = Some(next);
        unsafe { self.install(next) }
    }

    /// Repeat the retained switch and its native maintenance on the same Hart.
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
        unsafe {
            asm!("mov cr3, {next}", next = in(reg) next.cr3, options(nostack, preserves_flags));
        }
        if unsafe { Controls::current() } != next {
            return Err(SwitchError::Unconfirmed);
        }
        self.approved = next;
        self.pending = None;
        Ok(())
    }
}
