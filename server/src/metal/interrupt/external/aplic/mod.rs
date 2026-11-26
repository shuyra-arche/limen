//! Advanced Platform-Level Interrupt Controller: supervisor direct delivery.

use crate::metal::{
    platform::description::devicetree::{DeviceTree, Error, Node, NodeId, cell},
    processor::hart::riscv::description::RiscvCpus,
};
use limen_treaty::model::PhysicalRange;

const SOURCE_LIMIT: u32 = 1023;
const IDC_BASE: usize = 0x4000;
const IDC_BYTES: usize = 32;

#[cfg(all(limen_target, target_arch = "riscv64"))]
mod direct;
#[cfg(all(limen_target, target_arch = "riscv64"))]
pub use direct::{Claim, Direct, Failure};

/// The checked physical domain and exact direct-delivery target, not authority.
#[derive(Clone, Copy, Debug)]
pub struct Description<'a> {
    source: NodeId<'a>,
    registers: PhysicalRange,
    hart: u64,
    index: usize,
    contexts: usize,
    sources: u32,
}

impl<'a> Description<'a> {
    pub fn read(tree: &DeviceTree<'a>, hart: u64) -> Result<Option<Self>, Error> {
        let cpus = RiscvCpus::new(*tree)?.ok_or(Error::Missing)?;
        if !cpus.cpu(hart)?.is_some_and(|cpu| cpu.enabled()) {
            return Err(Error::Missing);
        }
        let mut found = None;
        for node in tree.nodes() {
            if !node.compatible(b"riscv,aplic")? || !node.enabled()? {
                continue;
            }
            if node.property(b"msi-parent")?.is_some() {
                continue;
            }
            let Some(bytes) = node.property(b"interrupts-extended")? else {
                continue;
            };
            let first = bytes.get(4..8).ok_or(Error::Format)?;
            let cause = u32::from_be_bytes(first.try_into().unwrap());
            if cause != 9 {
                continue;
            }
            let Some((index, contexts)) = cpus.interrupt_index(bytes, 9, hart)? else {
                continue;
            };
            if node.property(b"interrupt-controller")? != Some(b"")
                || node.property(b"#interrupt-cells")? != Some(&2u32.to_be_bytes())
            {
                return Err(Error::Unsupported);
            }
            let sources = cell(node.property(b"riscv,num-sources")?)?.ok_or(Error::Missing)?;
            let registers = node.register(0)?;
            if !(1..=SOURCE_LIMIT).contains(&sources)
                || !registers.start().get().is_multiple_of(4096)
                || registers.length().get() < (IDC_BASE + contexts * IDC_BYTES) as u64
            {
                return Err(Error::Range);
            }
            if found.is_some() {
                return Err(Error::Conflict);
            }
            found = Some(Self {
                source: node.id(),
                registers,
                hart,
                index,
                contexts,
                sources,
            });
        }
        Ok(found)
    }

    pub fn source(self) -> NodeId<'a> {
        self.source
    }

    pub fn registers(self) -> PhysicalRange {
        self.registers
    }

    pub fn hart(self) -> u64 {
        self.hart
    }

    pub fn index(self) -> usize {
        self.index
    }

    pub fn contexts(self) -> usize {
        self.contexts
    }

    pub fn sources(self) -> u32 {
        self.sources
    }

    /// Check the device's actual two-cell binding to this exact domain.
    /// This bounded profile supports an explicit parent and one interrupt.
    pub fn route(self, tree: &DeviceTree<'a>, device: Node<'_, 'a>) -> Result<Route<'a>, Error> {
        if tree.node(device.id()).is_none() || tree.node(self.source).is_none() {
            return Err(Error::Missing);
        }
        if !device.enabled()? || device.property(b"interrupts-extended")?.is_some() {
            return Err(Error::Unsupported);
        }
        let parent = cell(device.property(b"interrupt-parent")?)?.ok_or(Error::Missing)?;
        if tree.phandle(parent)?.ok_or(Error::Missing)?.id() != self.source {
            return Err(Error::Conflict);
        }
        let bytes = device.property(b"interrupts")?.ok_or(Error::Missing)?;
        let [id, flags] = *bytes
            .as_chunks::<4>()
            .0
            .first_chunk::<2>()
            .ok_or(Error::Format)?;
        if bytes.len() != 8 {
            return Err(Error::Unsupported);
        }
        let id = u32::from_be_bytes(id);
        if !(1..=self.sources).contains(&id) {
            return Err(Error::Range);
        }
        let mode = match u32::from_be_bytes(flags) {
            1 => Mode::EdgeRising,
            2 => Mode::EdgeFalling,
            4 => Mode::LevelHigh,
            8 => Mode::LevelLow,
            _ => return Err(Error::Unsupported),
        };
        Ok(Route {
            domain: self.source,
            source: device.id(),
            id,
            mode,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Mode {
    EdgeRising = 4,
    EdgeFalling = 5,
    LevelHigh = 6,
    LevelLow = 7,
}

/// A checked device route description; actual programming requires control.
#[derive(Clone, Copy, Debug)]
pub struct Route<'a> {
    domain: NodeId<'a>,
    source: NodeId<'a>,
    id: u32,
    mode: Mode,
}

impl<'a> Route<'a> {
    pub fn domain(self) -> NodeId<'a> {
        self.domain
    }

    pub fn source(self) -> NodeId<'a> {
        self.source
    }

    pub fn id(self) -> u32 {
        self.id
    }

    pub fn mode(self) -> Mode {
        self.mode
    }
}
