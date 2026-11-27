//! Native time-register access on the original Hart.

use crate::metal::platform::description::devicetree::NodeId;

use super::{
    super::treaty::{Clock, Deadline},
    description::RiscvTimebase,
};

/// Actual read access to the original counter on one admitted Hart.
#[cfg(all(limen_target, target_arch = "riscv64"))]
#[must_use]
pub struct Counter<'a> {
    timebase: RiscvTimebase<'a>,
    clock: Clock,
}

#[cfg(all(limen_target, target_arch = "riscv64"))]
impl<'a> Counter<'a> {
    /// # Safety
    /// The executing Hart has actual time access and the trusted platform
    /// establishes its relationship to the described source. These conditions
    /// must remain valid throughout this use.
    pub unsafe fn admit(timebase: RiscvTimebase<'a>, hart: u64) -> Option<Self> {
        let hart = crate::metal::processor::treaty::try_current_processor()
            .filter(|local| local.get() as u64 == hart)?;
        Some(Self {
            timebase,
            clock: Clock::bind(hart, timebase.hertz.into(), read_native)?,
        })
    }

    pub fn clock(&self) -> &Clock {
        &self.clock
    }

    pub fn source(&self) -> NodeId<'a> {
        self.timebase.source()
    }

    pub fn hart(&self) -> u64 {
        self.clock.processor() as u64
    }

    pub fn hertz(&self) -> u32 {
        self.timebase.hertz()
    }

    pub fn deadline(&self, count: u64) -> Deadline<'_> {
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
