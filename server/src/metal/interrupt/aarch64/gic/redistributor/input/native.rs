//! Private interrupt inputs of one qualified PE's Redistributor.

use core::{arch::asm, marker::PhantomData};

use super::super::{Failure, Frame};
use super::{
    Setup, State,
    description::{PrivateInterrupt, PrivateInterrupts},
};
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
enum PpiState {
    Reserved,
    Ready,
    Producing,
    Stopped,
    Completed,
}

/// Linear permission for one reserved input in its original GICR frame.
#[derive(Debug)]
#[must_use]
pub struct Ppi<'tree, 'map> {
    source: NodeId<'tree>,
    domain: NodeId<'tree>,
    redistributor: PhysicalRange,
    physical: PhysicalRange,
    affinity: usize,
    intid: u32,
    state: PpiState,
    mapping: PhantomData<&'map mut ()>,
}

impl<'tree, 'map> Ppi<'tree, 'map> {
    pub fn source(&self) -> NodeId<'tree> {
        self.source
    }

    pub fn domain(&self) -> NodeId<'tree> {
        self.domain
    }

    pub fn intid(&self) -> u32 {
        self.intid
    }

    pub(in crate::metal::interrupt::aarch64) fn local_to(&self, processor: usize) -> bool {
        self.affinity == processor && local(self.affinity)
    }

    pub(in crate::metal::interrupt::aarch64::gic) fn matches(
        &self,
        frame: &Frame<'_>,
        intid: u32,
    ) -> bool {
        frame.ppi_index(self).is_ok() && self.intid == intid
    }

    pub(in crate::metal::interrupt::aarch64) fn permits_programming(
        &self,
        replacement: bool,
    ) -> bool {
        self.state
            == if replacement {
                PpiState::Producing
            } else {
                PpiState::Ready
            }
    }

    pub(in crate::metal::interrupt::aarch64) fn producing(&mut self) {
        self.state = PpiState::Producing;
    }

    /// Called by the exact producer after its native stop is confirmed.
    pub(in crate::metal::interrupt::aarch64) fn stopped(&mut self) {
        debug_assert_eq!(self.state, PpiState::Producing);
        self.state = PpiState::Stopped;
    }

    pub(in crate::metal::interrupt::aarch64) fn stopped_input(
        &mut self,
    ) -> Option<StoppedPpi<'_, 'tree, 'map>> {
        (self.state == PpiState::Stopped).then_some(StoppedPpi(self))
    }
}

/// The exact PPI of a confirmed stopped native producer.
#[must_use]
pub struct StoppedPpi<'input, 'tree, 'map>(&'input mut Ppi<'tree, 'map>);

impl StoppedPpi<'_, '_, '_> {
    pub(in crate::metal::interrupt::aarch64::gic) fn matches(
        &self,
        frame: &Frame<'_>,
        intid: u32,
    ) -> bool {
        self.0.matches(frame, intid)
    }

    pub(in crate::metal::interrupt::aarch64::gic) fn complete(self) {
        self.0.state = PpiState::Completed;
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
    ) -> Result<Ppi<'tree, 'map>, Failure> {
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
            redistributor: self.domain(),
            physical: self.physical(),
            affinity: self.affinity(),
            intid: id,
            state: PpiState::Reserved,
            mapping: PhantomData,
        })
    }

    pub(in crate::metal::interrupt::aarch64::gic) fn prepare_ppi(
        &mut self,
        ppi: &mut Ppi<'_, '_>,
    ) -> Result<(), Failure> {
        let index = self.ppi_index(ppi)?;
        match ppi.state {
            PpiState::Reserved => {
                if !matches!(self.private[index], State::Enabled(_)) {
                    self.enable_input(ppi.intid)?;
                }
            }
            PpiState::Completed => (),
            _ => return Err(Failure::Busy),
        }
        let bit = 1 << ppi.intid;
        if !matches!(self.private[index], State::Enabled(_))
            || self.private_word(ENABLE) & bit == 0
            || (self.private_word(PENDING) | self.private_word(ACTIVE)) & bit != 0
        {
            return Err(Failure::Busy);
        }
        ppi.state = PpiState::Ready;
        Ok(())
    }

    fn reserve_input(&mut self, id: u32, priority: u8) -> Result<(), Failure> {
        let index = id as usize;
        let bit = 1 << id;
        if !self.private_quiet(bit) {
            return Err(Failure::Busy);
        }
        let setup = match self.private[index] {
            State::Vacant => Setup {
                priority,
                inherited_priority: self.private_priority(id),
                group: self.private_word(GROUP) & bit != 0,
            },
            State::Configuring(setup) if setup.priority == priority => setup,
            _ => return Err(Failure::Busy),
        };
        self.private[index] = State::Configuring(setup);
        self.write_group(bit, true);
        self.write_priority(id, setup.priority);
        synchronize();
        if self.private_word(GROUP) & bit == 0 || self.private_priority(id) != setup.priority {
            return Err(Failure::Busy);
        }
        self.private[index] = State::Masked(setup);
        Ok(())
    }

    fn enable_input(&mut self, id: u32) -> Result<(), Failure> {
        let index = id as usize;
        let setup = match self.private[index] {
            State::Masked(setup) | State::Enabling(setup) => setup,
            _ => return Err(Failure::Busy),
        };
        let bit = 1 << id;
        self.private[index] = State::Enabling(setup);
        self.write_private(ENABLE, bit);
        synchronize();
        if self.private_word(ENABLE) & bit == 0 {
            return Err(Failure::Busy);
        }
        self.private[index] = State::Enabled(setup);
        Ok(())
    }

    /// # Safety
    /// The producing source is stopped and the original CPU Interface,
    /// priority drop and deactivation are all settled. Keep the source
    /// stopped through reacquisition. A refusal returns the exact lease.
    pub unsafe fn retire_ppi<'tree, 'origin>(
        &mut self,
        ppi: Ppi<'tree, 'origin>,
    ) -> Result<(), (Ppi<'tree, 'origin>, Failure)> {
        if matches!(ppi.state, PpiState::Producing | PpiState::Stopped) {
            return Err((ppi, Failure::Busy));
        }
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
            State::Masked(setup) | State::Enabling(setup) if setup.priority == priority => (),
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

    pub(in crate::metal::interrupt::aarch64::gic) fn signal_priority(
        &mut self,
        id: u8,
    ) -> Option<u8> {
        if id >= 16 {
            return None;
        }
        let State::Enabled(setup) = self.private[usize::from(id)] else {
            return None;
        };
        let bit = 1 << id;
        (self.private_word(ENABLE) & bit != 0
            && self.private_word(GROUP) & bit != 0
            && self.private_priority(id.into()) == setup.priority)
            .then_some(setup.priority)
    }

    pub(in crate::metal::interrupt::aarch64::gic) fn signal_pending(&mut self, id: u8) -> bool {
        self.private_word(PENDING) & (1 << id) != 0
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
        let setup = self.private[index].retirement().ok_or(Failure::Busy)?;
        let bit = 1 << id;
        if self.private[index] != State::Restoring(setup) {
            self.private[index] = State::Masking(setup);
            self.write_private(DISABLE, bit);
            synchronize();
            if !self.private_quiet(bit) {
                return Err(Failure::Busy);
            }
            self.private[index] = State::Restoring(setup);
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
        self.private[index] = State::Vacant;
        Ok(())
    }

    fn ppi_index(&self, ppi: &Ppi<'_, '_>) -> Result<usize, Failure> {
        if ppi.physical != self.physical()
            || ppi.redistributor != self.domain()
            || !ppi.local_to(self.affinity())
        {
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
