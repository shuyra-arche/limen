//! One device's PLIC route into an exclusive supervisor context.

use crate::metal::{
    platform::description::devicetree::{DeviceTree, Error, Node, NodeId, cell},
    processor::hart::riscv64::description::RiscvCpus,
};
use limen_treaty::model::PhysicalRange;

mod context;
pub use context::{Barrier, Claim, Context, Failure, State};

const SOURCES: u32 = 1023;
const CONTEXTS: usize = 15872;
const ENABLE: usize = 0x2000;
const ENABLE_STRIDE: usize = 128;
const CONTEXT: usize = 0x20_0000;
const CONTEXT_STRIDE: usize = 4096;

/// The original device, domain, source and dense context index from its wiring.
#[derive(Clone, Copy, Debug)]
pub struct Route<'a> {
    device: NodeId<'a>,
    domain: NodeId<'a>,
    registers: PhysicalRange,
    hart: u64,
    index: usize,
    source: u32,
    sources: u32,
}

impl<'a> Route<'a> {
    /// The single-source DT binding with an explicit interrupt-parent.
    pub fn read(
        tree: &DeviceTree<'a>,
        cpus: &RiscvCpus<'a>,
        device: Node<'_, 'a>,
        hart: u64,
    ) -> Result<Self, Error> {
        if tree.node(cpus.node().id()).is_none() || tree.node(device.id()).is_none() {
            return Err(Error::Conflict);
        }
        if !device.enabled()? || !cpus.cpu(hart)?.is_some_and(|cpu| cpu.enabled()) {
            return Err(Error::Missing);
        }
        if device.property(b"interrupts-extended")?.is_some() {
            return Err(Error::Unsupported);
        }
        let parent = cell(device.property(b"interrupt-parent")?)?.ok_or(Error::Missing)?;
        let domain = tree.phandle(parent)?.ok_or(Error::Missing)?;
        if !domain.enabled()?
            || !(domain.compatible(b"riscv,plic0")? || domain.compatible(b"sifive,plic-1.0.0")?)
            || domain.property(b"interrupt-controller")? != Some(b"")
            || cell(domain.property(b"#interrupt-cells")?)? != Some(1)
        {
            return Err(Error::Unsupported);
        }
        let source = cell(device.property(b"interrupts")?)?.ok_or(Error::Missing)?;
        let sources = cell(domain.property(b"riscv,ndev")?)?.ok_or(Error::Missing)?;
        if !(1..=SOURCES).contains(&sources) || !(1..=sources).contains(&source) {
            return Err(Error::Range);
        }
        let bytes = domain
            .property(b"interrupts-extended")?
            .ok_or(Error::Missing)?;
        let (pairs, tail) = bytes.as_chunks::<8>();
        if pairs.is_empty() || !tail.is_empty() {
            return Err(Error::Format);
        }
        if pairs.len() > CONTEXTS {
            return Err(Error::Capacity);
        }
        let mut found = None;
        for (index, pair) in pairs.iter().enumerate() {
            let cause = u32::from_be_bytes(pair[4..].try_into().unwrap());
            // Machine or firmware-disabled contexts still occupy real indices.
            if matches!(cause, 11 | u32::MAX) {
                continue;
            }
            if cause != 9 {
                return Err(Error::Unsupported);
            }
            let handle = u32::from_be_bytes(pair[..4].try_into().unwrap());
            if cpus.interrupt_hart(handle)? == hart && found.replace(index).is_some() {
                return Err(Error::Conflict);
            }
        }
        let index = found.ok_or(Error::Missing)?;
        let registers = domain.register(0)?;
        if !registers.start().get().is_multiple_of(4096)
            || registers.length().get() < (CONTEXT + index * CONTEXT_STRIDE + 8) as u64
        {
            return Err(Error::Range);
        }
        Ok(Self {
            device: device.id(),
            domain: domain.id(),
            registers,
            hart,
            index,
            source,
            sources,
        })
    }
    pub fn device(self) -> NodeId<'a> {
        self.device
    }
    pub fn domain(self) -> NodeId<'a> {
        self.domain
    }
    pub fn registers(self) -> PhysicalRange {
        self.registers
    }
    pub fn source(self) -> u32 {
        self.source
    }
    pub fn context(self) -> usize {
        self.index
    }
    fn threshold(self) -> usize {
        CONTEXT + self.index * CONTEXT_STRIDE
    }
    fn enables(self) -> usize {
        ENABLE + self.index * ENABLE_STRIDE
    }
    fn enable(self) -> usize {
        self.enables() + (self.source as usize / 32) * 4
    }
    fn bit(self) -> u32 {
        1 << (self.source % 32)
    }
    fn priority(self) -> usize {
        self.source as usize * 4
    }
}
