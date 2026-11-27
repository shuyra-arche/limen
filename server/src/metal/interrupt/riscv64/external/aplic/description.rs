//! Firmware describes the physical domain, its targets and device routes.

use crate::metal::{
    platform::description::devicetree::{DeviceTree, Error, Node, NodeId, cell},
    processor::description::riscv64::cpu::RiscvCpus,
};
use limen_treaty::model::PhysicalRange;

use super::{
    SOURCE_LIMIT,
    direct::{self, Destination},
};

/// The checked physical domain and exact direct-delivery target, not authority.
#[derive(Clone, Copy, Debug)]
pub struct Description<'a> {
    domain: DomainDescription<'a>,
    destination: Destination,
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
                || registers.length().get() < (direct::BASE + contexts * direct::BYTES) as u64
            {
                return Err(Error::Range);
            }
            if found.is_some() {
                return Err(Error::Conflict);
            }
            found = Some(Self {
                domain: DomainDescription {
                    source: node.id(),
                    registers,
                    sources,
                },
                destination: Destination::new(hart, index, contexts),
            });
        }
        Ok(found)
    }

    pub fn domain(self) -> DomainDescription<'a> {
        self.domain
    }

    pub fn destination(self) -> Destination {
        self.destination
    }
}

/// Checked aperture and source geometry of one physical domain.
#[derive(Clone, Copy, Debug)]
pub struct DomainDescription<'a> {
    source: NodeId<'a>,
    registers: PhysicalRange,
    sources: u32,
}

impl<'a> DomainDescription<'a> {
    pub fn source(self) -> NodeId<'a> {
        self.source
    }

    pub fn registers(self) -> PhysicalRange {
        self.registers
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
pub enum Mode {
    EdgeRising,
    EdgeFalling,
    LevelHigh,
    LevelLow,
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
