//! Firmware's description of a Hart's maximum translation mode.

use super::Mode;
use crate::metal::{
    firmware::devicetree::{Error, NodeId},
    hart::description::RiscvCpu,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RiscvMmuMode {
    Bare,
    Sv39,
    Sv48,
    Sv57,
}

impl RiscvMmuMode {
    /// Whether the firmware's maximum includes this paging mode. The
    /// calling Hart still has to establish the actual SATP configuration.
    pub fn supports(self, mode: Mode) -> bool {
        let maximum = match self {
            Self::Bare => return false,
            Self::Sv39 => Mode::Sv39,
            Self::Sv48 => Mode::Sv48,
            Self::Sv57 => Mode::Sv57,
        };
        mode <= maximum
    }
}

#[derive(Clone, Copy)]
pub struct RiscvMmu<'a> {
    source: NodeId<'a>,
    maximum: RiscvMmuMode,
}

impl<'a> RiscvMmu<'a> {
    /// Interpret `mmu-type`. The result is a description of capacity.
    pub fn describe(cpu: &RiscvCpu<'_, 'a>) -> Result<Option<Self>, Error> {
        let cpu = cpu.node();
        let Some(property) = cpu.string(b"mmu-type")? else {
            return Ok(None);
        };
        let maximum = match property {
            b"riscv,none" => RiscvMmuMode::Bare,
            b"riscv,sv39" => RiscvMmuMode::Sv39,
            b"riscv,sv48" => RiscvMmuMode::Sv48,
            b"riscv,sv57" => RiscvMmuMode::Sv57,
            _ => return Err(Error::Unsupported),
        };
        Ok(Some(Self {
            source: cpu.id(),
            maximum,
        }))
    }

    pub fn source(self) -> NodeId<'a> {
        self.source
    }

    pub fn maximum(self) -> RiscvMmuMode {
        self.maximum
    }
}
