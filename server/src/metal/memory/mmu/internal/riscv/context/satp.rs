//! Local Hart translation control through the supervisor SATP register.

use core::{arch::asm, marker::PhantomData};

use super::{super::description::RiscvMmu, Context};
use crate::metal::processor::hart::local::try_current_hart;

impl Context {
    pub fn current() -> Option<Self> {
        Self::from_bits(read())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SwitchError {
    Scope,
    Context,
    Current,
    Rejected,
}

/// Exclusive access to the local SATP control, not ownership of a root.
/// The actual Hart and firmware maximum are retained independently of the
/// current Context, which is re-read at each switch.
#[must_use]
pub struct Translation<'a> {
    description: RiscvMmu<'a>,
    local: PhantomData<*mut ()>,
}

impl<'a> Translation<'a> {
    /// # Safety
    /// The caller controls SATP exclusively on this described Hart and
    /// establishes the actual supervisor environment. Preserve this
    /// qualification through all switches and old translation obligations.
    pub unsafe fn admit(description: RiscvMmu<'a>) -> Result<Self, SwitchError> {
        let access = Self {
            description,
            local: PhantomData,
        };
        access.check()?;
        let current = Context::current().ok_or(SwitchError::Current)?;
        if current
            .mode()
            .is_some_and(|mode| !description.maximum().supports(mode))
        {
            return Err(SwitchError::Scope);
        }
        Ok(access)
    }

    fn check(&self) -> Result<(), SwitchError> {
        if !try_current_hart().is_some_and(|hart| hart.get() as u64 == self.description.hart()) {
            return Err(SwitchError::Context);
        }
        let status: usize;
        unsafe { asm!("csrr {}, sstatus", out(reg) status, options(nomem, nostack)) };
        if status & 2 != 0 {
            return Err(SwitchError::Context);
        }
        Ok(())
    }

    /// Install the next context, preserving the original local fence order.
    ///
    /// # Safety
    /// The next root must preserve all live code, stack, trap and data paths
    /// with its table stores visible here. The caller separately owns
    /// instruction visibility, remote synchronization and old-root retirement.
    pub unsafe fn switch(&mut self, current: Context, next: Context) -> Result<(), SwitchError> {
        self.check()?;
        if next
            .mode()
            .is_some_and(|mode| !self.description.maximum().supports(mode))
        {
            return Err(SwitchError::Scope);
        }
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
}

fn read() -> u64 {
    let bits;
    unsafe {
        asm!("csrr {}, satp", out(reg) bits, options(nostack, nomem));
    }
    bits
}
