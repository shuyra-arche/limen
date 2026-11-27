//! Firmware's description of a Hart's maximum translation mode.

use super::Mode;
use crate::metal::{
    platform::description::devicetree::{Error, NodeId},
    processor::hart::riscv64::description::RiscvCpu,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MaximumMode {
    Bare,
    Paged(Mode),
}

impl MaximumMode {
    /// Whether the firmware's maximum includes this paging mode. The
    /// calling Hart still has to establish the actual SATP configuration.
    pub fn supports(self, mode: Mode) -> bool {
        let maximum = match self {
            Self::Bare => return false,
            Self::Paged(mode) => mode,
        };
        mode <= maximum
    }
}

#[derive(Clone, Copy)]
pub struct RiscvMmu<'a> {
    source: NodeId<'a>,
    hart: u64,
    maximum: MaximumMode,
}

impl<'a> RiscvMmu<'a> {
    /// Interpret `mmu-type`. The result is a description of capacity.
    pub fn describe(cpu: &RiscvCpu<'_, 'a>) -> Result<Option<Self>, Error> {
        let hart = cpu.id();
        let cpu = cpu.node();
        let Some(property) = cpu.string(b"mmu-type")? else {
            return Ok(None);
        };
        let maximum = match property {
            b"riscv,none" => MaximumMode::Bare,
            b"riscv,sv39" => MaximumMode::Paged(Mode::Sv39),
            b"riscv,sv48" => MaximumMode::Paged(Mode::Sv48),
            b"riscv,sv57" => MaximumMode::Paged(Mode::Sv57),
            _ => return Err(Error::Unsupported),
        };
        Ok(Some(Self {
            source: cpu.id(),
            hart,
            maximum,
        }))
    }

    pub fn source(self) -> NodeId<'a> {
        self.source
    }

    pub fn hart(self) -> u64 {
        self.hart
    }

    pub fn maximum(self) -> MaximumMode {
        self.maximum
    }
}
