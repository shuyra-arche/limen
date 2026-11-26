//! The RISC-V timebase frequency declared for the source CPU scope.

use core::num::NonZeroU32;

use crate::metal::{
    platform::description::devicetree::{Error, NodeId},
    processor::hart::riscv::description::RiscvCpus,
};

#[derive(Clone, Copy)]
pub struct RiscvTimebase<'a> {
    source: NodeId<'a>,
    hertz: NonZeroU32,
}

impl<'a> RiscvTimebase<'a> {
    pub fn describe(cpus: &RiscvCpus<'a>) -> Result<Option<Self>, Error> {
        let cpus = cpus.node();
        let Some(property) = cpus.property(b"timebase-frequency")? else {
            return Ok(None);
        };
        let encoded = property.try_into().map_err(|_| Error::Format)?;
        let hertz = NonZeroU32::new(u32::from_be_bytes(encoded)).ok_or(Error::Range)?;
        Ok(Some(Self {
            source: cpus.id(),
            hertz,
        }))
    }

    pub fn source(self) -> NodeId<'a> {
        self.source
    }

    pub fn hertz(self) -> u32 {
        self.hertz.get()
    }
}

/// Actual read access to the original counter on one admitted Hart.
#[cfg(all(limen_target, target_arch = "riscv64"))]
#[must_use]
pub struct Counter<'a> {
    timebase: RiscvTimebase<'a>,
    hart: u64,
    local: core::marker::PhantomData<*mut ()>,
}

#[cfg(all(limen_target, target_arch = "riscv64"))]
impl<'a> Counter<'a> {
    /// # Safety
    /// The executing Hart has actual time access and the trusted platform
    /// establishes its relationship to the described source. These conditions
    /// must remain valid throughout this use.
    pub unsafe fn admit(timebase: RiscvTimebase<'a>, hart: u64) -> Option<Self> {
        if crate::metal::processor::hart::local::current_hart().get() as u64 != hart {
            return None;
        }
        Some(Self {
            timebase,
            hart,
            local: core::marker::PhantomData,
        })
    }

    pub fn source(&self) -> NodeId<'a> {
        self.timebase.source()
    }

    pub fn hart(&self) -> u64 {
        self.hart
    }

    pub fn hertz(&self) -> u32 {
        self.timebase.hertz()
    }

    pub fn read(&self) -> Option<u64> {
        if crate::metal::processor::hart::local::current_hart().get() as u64 != self.hart {
            return None;
        }
        let count;
        unsafe { core::arch::asm!("rdtime {}", out(reg) count, options(nostack, nomem)) };
        Some(count)
    }
}
