//! Supervisor notification scope and the original SBI issuance record.

use crate::metal::{
    interrupt::treaty::Issuance, platform::description::devicetree::NodeId,
    processor::description::riscv64::cpu::RiscvCpu,
};

#[cfg(all(limen_target, target_arch = "riscv64", limen_firmware = "sbi"))]
mod native;
#[cfg(all(limen_target, target_arch = "riscv64", limen_firmware = "sbi"))]
pub use native::{IpiRoute, Receiver, Sender};

#[derive(Clone, Copy, Eq, PartialEq)]
struct Source<'tree> {
    scope: NodeId<'tree>,
    hart: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Scope,
    Busy,
    Unconfirmed,
    Firmware(isize),
}

/// One discovered destination in the firmware CPU scope.
#[cfg_attr(
    not(all(limen_target, target_arch = "riscv64", limen_firmware = "sbi")),
    expect(
        dead_code,
        reason = "native SBI admission checks the original target scope and Hart"
    )
)]
pub struct Target<'tree> {
    scope: NodeId<'tree>,
    hart: usize,
}

impl<'tree> Target<'tree> {
    pub fn describe(cpu: &RiscvCpu<'_, 'tree>) -> Option<Self> {
        cpu.enabled().then_some(())?;
        Some(Self {
            scope: cpu.node().parent_id()?,
            hart: usize::try_from(cpu.id()).ok()?,
        })
    }
}

/// Resident issuance record retained by the original Hart owner.
#[cfg_attr(
    not(any(
        test,
        all(limen_target, target_arch = "riscv64", limen_firmware = "sbi")
    )),
    expect(
        dead_code,
        reason = "native SBI issuance uses the original resident record"
    )
)]
pub struct SendState<'tree> {
    #[cfg_attr(
        all(
            test,
            not(all(limen_target, target_arch = "riscv64", limen_firmware = "sbi"))
        ),
        expect(dead_code, reason = "native SBI admission binds the source")
    )]
    source: Option<Source<'tree>>,
    unconfirmed: Option<(usize, isize)>,
}

impl<'tree> SendState<'tree> {
    pub const FRESH: Self = Self {
        source: None,
        unconfirmed: None,
    };
}

#[cfg_attr(
    not(any(
        test,
        all(limen_target, target_arch = "riscv64", limen_firmware = "sbi")
    )),
    expect(
        dead_code,
        reason = "native SBI issuance advances the original resident record"
    )
)]
impl<'tree> SendState<'tree> {
    #[cfg_attr(
        all(
            test,
            not(all(limen_target, target_arch = "riscv64", limen_firmware = "sbi"))
        ),
        expect(
            dead_code,
            reason = "native SBI admission establishes the original source"
        )
    )]
    fn admit(&mut self, source: Source<'tree>) -> Result<(), Error> {
        if self.source.is_some_and(|original| original != source) {
            return Err(Error::Scope);
        }
        self.source = Some(source);
        Ok(())
    }

    fn require_available(&self) -> Result<(), Error> {
        self.unconfirmed.is_none().then_some(()).ok_or(Error::Busy)
    }

    fn retain_unconfirmed(&mut self, hart: usize, error: isize) {
        debug_assert!(self.unconfirmed.is_none());
        self.unconfirmed = Some((hart, error));
    }

    fn pending(&self, hart: usize) -> Result<Option<Issuance<Error>>, Error> {
        match self.unconfirmed {
            Some((original, error)) if original == hart => {
                Ok(Some(Issuance::Unconfirmed(Error::Firmware(error))))
            }
            Some(_) => Err(Error::Busy),
            None => Ok(None),
        }
    }
}

#[cfg(test)]
#[path = "../../../../../../tests/metal/interrupt/riscv64/internal/ipi.rs"]
mod tests;
