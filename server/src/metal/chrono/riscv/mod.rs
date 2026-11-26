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

#[cfg(all(limen_target, target_arch = "riscv64"))]
impl RiscvTimebase<'_> {
    /// Read the current architectural time counter, not a calibrated Instant.
    ///
    /// # Safety
    /// The execution environment must grant this Hart access to time and
    /// establish its relationship to the described timebase.
    pub unsafe fn counter(self) -> u64 {
        let count;
        unsafe { core::arch::asm!("rdtime {}", out(reg) count, options(nostack, nomem)) };
        count
    }
}
