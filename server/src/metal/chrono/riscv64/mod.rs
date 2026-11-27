//! The RISC-V timebase frequency declared for the source CPU scope.

use core::num::NonZeroU32;

use crate::metal::{
    platform::description::devicetree::{Error, NodeId},
    processor::hart::riscv64::description::RiscvCpus,
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
    clock: super::Clock<'a>,
}

#[cfg(all(limen_target, target_arch = "riscv64"))]
impl<'a> Counter<'a> {
    /// # Safety
    /// The executing Hart has actual time access and the trusted platform
    /// establishes its relationship to the described source. These conditions
    /// must remain valid throughout this use.
    pub unsafe fn admit(timebase: RiscvTimebase<'a>, hart: u64) -> Option<Self> {
        let hart = usize::try_from(hart).ok()?;
        Some(Self {
            clock: super::Clock::bind(timebase.source, hart, timebase.hertz, read_native)?,
        })
    }

    pub fn clock(&self) -> &super::Clock<'a> {
        &self.clock
    }

    pub fn source(&self) -> NodeId<'a> {
        self.clock.source()
    }

    pub fn hart(&self) -> u64 {
        self.clock.hart() as u64
    }

    pub fn hertz(&self) -> u32 {
        self.clock.frequency().get()
    }

    pub fn deadline(&self, count: u64) -> super::Deadline<'_, 'a> {
        self.clock.deadline(count)
    }

    pub fn read(&self) -> Option<u64> {
        self.clock.read()
    }
}

#[cfg(all(limen_target, target_arch = "riscv64"))]
fn read_native() -> u64 {
    let count;
    unsafe { core::arch::asm!("rdtime {}", out(reg) count, options(nostack, nomem)) };
    count
}
