use super::super::SwitchError;
use super::{Controls, Format};
use crate::metal::processor::hart::local::try_current_hart;
use core::arch::asm;
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
    pub fn local_format(self) -> Option<Format> {
        use core::arch::x86_64::__cpuid;
        if __cpuid(1).edx & (1 << 16) == 0 || __cpuid(0x8000_0000).eax < 0x8000_0008 {
            return None;
        }
        self.format(
            __cpuid(0x8000_0008).eax as u8,
            __cpuid(0x8000_0001).edx & (1 << 26) != 0,
        )
    }

    /// Install a plain WB root and invalidate this processor's non-global
    /// translations. Success covers no other processor or outstanding access.
    ///
    /// # Safety
    /// Keep exclusive, nonmigrating, interrupt-masked privileged ownership,
    /// and supply a visible resident four-level root preserving every live
    /// code, data, stack and exception path. Exclude memory encryption or
    /// other physical-address modifiers; retain old tables and obligations.
    pub unsafe fn switch(self, hart: usize, root: PhysicalAddress) -> Result<(), SwitchError> {
        use crate::metal::processor::hart::x86_64::status;
        let flags: usize;
        unsafe { asm!("pushfq", "pop {}", out(reg) flags) };
        if !status::interrupts_masked(flags)
            || try_current_hart().is_none_or(|here| here.get() != hart)
        {
            return Err(SwitchError::Context);
        }
        let Some(format) = self.local_format() else {
            return Err(SwitchError::Scope);
        };
        if !root.get().is_multiple_of(4096) || root.get() >= (1 << format.physical_bits) {
            return Err(SwitchError::Scope);
        }
        if unsafe { Self::current() } != self {
            return Err(SwitchError::Current);
        }
        let installed: u64;
        unsafe {
            asm!("mov cr3, {next}", "mov {installed}, cr3", next = in(reg) root.get(),
                installed = lateout(reg) installed, options(nostack, preserves_flags));
        }
        (installed == root.get())
            .then_some(())
            .ok_or(SwitchError::Unconfirmed)
    }
}
