//! Machine timer description, with supervisor programming delegated to firmware.

use crate::metal::{
    platform::description::devicetree::{DeviceTree, Error, NodeId},
    processor::hart::riscv64::description::RiscvCpus,
};
use limen_treaty::model::{ByteLength, PhysicalRange};

#[cfg(limen_firmware = "sbi")]
mod supervisor;
#[cfg(limen_firmware = "sbi")]
pub use supervisor::{
    ClearStep, Clearing, Effect, Failure, Prior, ProgramStep, Programming, State, Timer,
};

/// MTIMER's original machine-level storage and the exact Hart comparator.
/// Neither range grants S-mode access.
#[derive(Clone, Copy, Debug)]
pub struct Description<'a> {
    source: NodeId<'a>,
    clock_scope: NodeId<'a>,
    hart: u64,
    counter: PhysicalRange,
    comparator: PhysicalRange,
}

impl<'a> Description<'a> {
    pub fn read(
        tree: &DeviceTree<'a>,
        cpus: &RiscvCpus<'a>,
        hart: u64,
    ) -> Result<Option<Self>, Error> {
        if tree.node(cpus.node().id()).is_none() {
            return Err(Error::Conflict);
        }
        if !cpus.cpu(hart)?.is_some_and(|cpu| cpu.enabled()) {
            return Err(Error::Missing);
        }
        let mut found = None;
        for node in tree.nodes() {
            // Machine-only nodes may be disabled for supervisor access by
            // firmware. Retain their original description, never their authority.
            if !node.compatible(b"riscv,aclint-mtimer")? {
                continue;
            }
            let Some(bytes) = node.property(b"interrupts-extended")? else {
                continue;
            };
            let Some((index, count)) = cpus.interrupt_index(bytes, 7, hart)? else {
                continue;
            };
            let mut registers = node.registers()?;
            if registers.len() != 2 {
                return Err(Error::Unsupported);
            }
            let counter_window = registers.next().unwrap()?;
            let comparators = registers.next().unwrap()?;
            if !counter_window.start().get().is_multiple_of(8)
                || counter_window.length().get() < 8
                || !comparators.start().get().is_multiple_of(8)
                || !comparators.length().get().is_multiple_of(8)
                || comparators.length().get() < (count * 8) as u64
                || counter_window.overlaps(comparators)
            {
                return Err(Error::Range);
            }
            // The first window may be larger than the single MTIME register.
            // Describe only the actual register, not every byte in the window.
            let counter = counter_window
                .start()
                .range(ByteLength::new(8))
                .ok_or(Error::Range)?;
            let comparator = comparators
                .start()
                .advance(ByteLength::new((index * 8) as u64))
                .and_then(|start| start.range(ByteLength::new(8)))
                .filter(|range| comparators.covers(*range))
                .ok_or(Error::Range)?;
            if found.is_some() {
                return Err(Error::Conflict);
            }
            found = Some(Self {
                source: node.id(),
                clock_scope: cpus.node().id(),
                hart,
                counter,
                comparator,
            });
        }
        Ok(found)
    }

    pub fn source(self) -> NodeId<'a> {
        self.source
    }

    pub fn clock_scope(self) -> NodeId<'a> {
        self.clock_scope
    }

    pub fn hart(self) -> u64 {
        self.hart
    }

    pub fn counter(self) -> PhysicalRange {
        self.counter
    }

    pub fn comparator(self) -> PhysicalRange {
        self.comparator
    }
}
