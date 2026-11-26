//! The RISC-V timebase frequency declared for the source CPU scope.

use core::num::NonZeroU32;

use crate::metal::{
    firmware::devicetree::{Error, NodeId},
    hart::riscv::description::RiscvCpus,
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
