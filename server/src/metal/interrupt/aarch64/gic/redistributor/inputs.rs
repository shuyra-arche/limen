//! Private interrupt inputs of one qualified PE's Redistributor.

use core::{arch::asm, marker::PhantomData};

use super::{Failure, Frame, PrivateInterrupt, PrivateInterrupts};
use crate::metal::{
    interrupt::aarch64::gic::{
        Trigger,
        cpu_interface::local,
        sgi::{Phase, Sgi},
    },
    platform::description::devicetree::NodeId,
};
use limen_treaty::model::PhysicalRange;

const SGI: usize = 0x1_0000;
const GROUP: usize = SGI + 0x80;
const ENABLE: usize = SGI + 0x100;
const DISABLE: usize = SGI + 0x180;
const PENDING: usize = SGI + 0x200;
const ACTIVE: usize = SGI + 0x300;
const PRIORITY: usize = SGI + 0x400;
const PPI_CONFIG: usize = SGI + 0xc04;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Setup {
    priority: u8,
    inherited_priority: u8,
    group: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PrivateState {
    Vacant,
    Configuring(Setup),
    Masked(Setup),
    Enabling(Setup),
    Enabled(Setup),
    Masking(Setup),
    Restoring(Setup),
}

/// Linear permission for one reserved input in its original GICR frame.
#[derive(Debug)]
#[must_use]
pub struct Ppi<'tree> {
    source: NodeId<'tree>,
    domain: NodeId<'tree>,
    physical: PhysicalRange,
    intid: u32,
}

impl<'tree> Ppi<'tree> {
    pub fn source(&self) -> NodeId<'tree> {
        self.source
    }

    pub fn domain(&self) -> NodeId<'tree> {
        self.domain
    }

    pub fn intid(&self) -> u32 {
        self.intid
    }
}

impl<'map> Frame<'map> {
    /// Reserve a level PPI whose previously admitted hardware use is closed.
    ///
    /// # Safety
    /// The committed firmware scope is the actual original source. It is
    /// quiesced, and the PE owns this input including its Group 1 control.
    /// Keep it quiesced until enable with a prepared CPU Interface. Only
    /// retry the identical request if readback is unconfirmed.
    pub unsafe fn reserve_ppi<'tree>(
        &mut self,
        interrupts: &PrivateInterrupts<'tree>,
        interrupt: PrivateInterrupt,
        priority: u8,
    ) -> Result<Ppi<'tree>, Failure> {
        let id = interrupt.intid;
        if !local(self.affinity())
            || self.domain() != interrupts.controller().redistributor()
            || !interrupts.entries().contains(&interrupt)
            || interrupt.trigger != Trigger::LevelHigh
            || !(16..32).contains(&id)
        {
            return Err(Failure::Scope);
        }
        let edge = 2 << ((id - 16) * 2);
        if self.private_word(PPI_CONFIG) & edge != 0 {
            return Err(Failure::Busy);
        }
        self.reserve_input(id, priority)?;
        Ok(Ppi {
            source: interrupts.source(),
            domain: interrupts.controller().source(),
            physical: self.physical(),
            intid: id,
        })
    }

    /// # Safety
    /// A qualified CPU Interface on this exact PE will preserve the original
    /// PPI's IAR and its mandatory native completion.
    pub unsafe fn enable_ppi(&mut self, ppi: &Ppi<'_>) -> Result<(), Failure> {
        self.ppi_index(ppi)?;
        self.enable_input(ppi.intid)
    }

    fn reserve_input(&mut self, id: u32, priority: u8) -> Result<(), Failure> {
        let index = id as usize;
        let bit = 1 << id;
        if !self.private_quiet(bit) {
            return Err(Failure::Busy);
        }
        let setup = match self.private[index] {
            PrivateState::Vacant => Setup {
                priority,
                inherited_priority: self.private_priority(id),
                group: self.private_word(GROUP) & bit != 0,
            },
            PrivateState::Configuring(setup) if setup.priority == priority => setup,
            _ => return Err(Failure::Busy),
        };
        self.private[index] = PrivateState::Configuring(setup);
        self.write_group(bit, true);
        self.write_priority(id, setup.priority);
        synchronize();
        if self.private_word(GROUP) & bit == 0 || self.private_priority(id) != setup.priority {
            return Err(Failure::Busy);
        }
        self.private[index] = PrivateState::Masked(setup);
        Ok(())
    }

    fn enable_input(&mut self, id: u32) -> Result<(), Failure> {
        let index = id as usize;
        let setup = match self.private[index] {
            PrivateState::Masked(setup) | PrivateState::Enabling(setup) => setup,
            _ => return Err(Failure::Busy),
        };
        let bit = 1 << id;
        self.private[index] = PrivateState::Enabling(setup);
        self.write_private(ENABLE, bit);
        synchronize();
        if self.private_word(ENABLE) & bit == 0 {
            return Err(Failure::Busy);
        }
        self.private[index] = PrivateState::Enabled(setup);
        Ok(())
    }

    pub(in crate::metal::interrupt::aarch64::gic) fn ready_ppi(&mut self, ppi: &Ppi<'_>) -> bool {
        let Ok(index) = self.ppi_index(ppi) else {
            return false;
        };
        let bit = 1 << ppi.intid;
        matches!(self.private[index], PrivateState::Enabled(_))
            && self.private_word(ENABLE) & bit != 0
            && (self.private_word(PENDING) | self.private_word(ACTIVE)) & bit == 0
    }

    /// # Safety
    /// The producing source is stopped and the original CPU Interface,
    /// priority drop and deactivation are all settled. Keep the source
    /// stopped through reacquisition. A refusal returns the exact lease.
    pub unsafe fn retire_ppi<'tree>(
        &mut self,
        ppi: Ppi<'tree>,
    ) -> Result<(), (Ppi<'tree>, Failure)> {
        self.ppi_index(&ppi)
            .and_then(|_| self.retire_input(ppi.intid))
            .map_err(|failure| (ppi, failure))
    }

    pub(in crate::metal::interrupt::aarch64::gic) fn prepare_signal(
        &mut self,
        id: u8,
        priority: u8,
    ) -> Result<Sgi<'map>, Failure> {
        if id >= 16 || !local(self.affinity()) {
            return Err(Failure::Scope);
        }
        let address =
            crate::metal::interrupt::aarch64::gic::sgi::selection(self.affinity() as u64, id)
                .ok_or(Failure::Scope)?;
        let index = usize::from(id);
        match self.private[index] {
            PrivateState::Masked(setup) | PrivateState::Enabling(setup)
                if setup.priority == priority =>
            {
                ()
            }
            _ => self.reserve_input(u32::from(id), priority)?,
        }
        self.enable_input(u32::from(id))?;
        Ok(Sgi {
            physical: self.physical(),
            domain: self.domain(),
            id,
            address,
            phase: Phase::Prepared,
            mapping: PhantomData,
        })
    }

    /// Restore the original private input after its exact remote claim settles.
    pub fn retire_sgi<'source>(
        &mut self,
        signal: Sgi<'source>,
    ) -> Result<(), (Sgi<'source>, Failure)> {
        if !local(self.affinity())
            || signal.physical != self.physical()
            || self.domain() != signal.domain
        {
            return Err((signal, Failure::Scope));
        }
        if signal.phase != Phase::Settled {
            return Err((signal, Failure::Busy));
        }
        self.retire_input(u32::from(signal.id))
            .map_err(|failure| (signal, failure))
    }

    fn retire_input(&mut self, id: u32) -> Result<(), Failure> {
        let index = id as usize;
        let setup = match self.private[index] {
            PrivateState::Masked(setup)
            | PrivateState::Enabling(setup)
            | PrivateState::Enabled(setup)
            | PrivateState::Masking(setup)
            | PrivateState::Restoring(setup) => setup,
            _ => return Err(Failure::Busy),
        };
        let bit = 1 << id;
        if self.private[index] != PrivateState::Restoring(setup) {
            self.private[index] = PrivateState::Masking(setup);
            self.write_private(DISABLE, bit);
            synchronize();
            if !self.private_quiet(bit) {
                return Err(Failure::Busy);
            }
            self.private[index] = PrivateState::Restoring(setup);
        }
        self.write_priority(id, setup.inherited_priority);
        self.write_group(bit, setup.group);
        synchronize();
        if (self.private_word(GROUP) & bit != 0) != setup.group
            || self.private_priority(id) != setup.inherited_priority
            || !self.private_quiet(bit)
        {
            return Err(Failure::Busy);
        }
        self.private[index] = PrivateState::Vacant;
        Ok(())
    }

    fn ppi_index(&self, ppi: &Ppi<'_>) -> Result<usize, Failure> {
        if ppi.physical != self.physical() || !local(self.affinity()) {
            return Err(Failure::Scope);
        }
        Ok(ppi.intid as usize)
    }

    fn private_word(&mut self, offset: usize) -> u32 {
        u32::from_le(unsafe { self.mapping.read_u32(offset) }.expect("checked GICR frame"))
    }

    fn write_private(&mut self, offset: usize, bits: u32) {
        unsafe { self.mapping.write_u32(offset, bits.to_le()) }.expect("checked GICR frame");
    }

    fn private_priority(&mut self, id: u32) -> u8 {
        unsafe { self.mapping.read_u8(PRIORITY + id as usize) }.expect("checked GICR priority")
    }

    fn write_priority(&mut self, id: u32, priority: u8) {
        unsafe { self.mapping.write_u8(PRIORITY + id as usize, priority) }
            .expect("checked GICR priority");
    }

    fn write_group(&mut self, bit: u32, group: bool) {
        let prior = self.private_word(GROUP);
        self.write_private(GROUP, if group { prior | bit } else { prior & !bit });
    }

    fn private_quiet(&mut self, bit: u32) -> bool {
        (self.private_word(ENABLE) | self.private_word(ACTIVE) | self.private_word(PENDING)) & bit
            == 0
    }
}

fn synchronize() {
    unsafe { asm!("dsb sy", options(nostack, preserves_flags)) };
}
