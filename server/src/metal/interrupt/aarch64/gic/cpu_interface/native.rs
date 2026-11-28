//! Native Group 1 acknowledge and completion on the original PE.

use core::arch::asm;

use super::super::{
    distributor::{Destination, RetirementReceipt, RetirementRequest},
    reception::{Failure, Mode, State},
    redistributor::{
        self, Frame,
        input::{Ppi, StoppedPpi},
    },
    sgi::{Phase, Sgi},
};
use crate::metal::{
    interrupt::treaty::{ClaimConfirmation, ClaimId, Completion, HardwareClaim},
    processor::{
        description::aarch64::status,
        executor::aarch64::native::boot_identity,
        treaty::{can_switch, wait_for_interrupt},
    },
};

pub(in crate::metal::interrupt::aarch64::gic) fn local(affinity: usize) -> bool {
    let el: usize;
    let mask: usize;
    unsafe {
        asm!("mrs {}, CurrentEL", "mrs {}, DAIF", out(reg) el, out(reg) mask, options(nostack, nomem))
    };
    status::el1(el) && mask & 0xc0 == 0xc0 && boot_identity() == affinity
}

/// Inspect the executing PE's architectural GIC CPU Interface feature.
///
/// # Safety
/// Run at an exception level allowed to read ID_AA64PFR0_EL1.
pub unsafe fn available() -> bool {
    let features: u64;
    unsafe { asm!("mrs {}, ID_AA64PFR0_EL1", out(reg) features, options(nomem, nostack)) };
    super::system_register_interface(features)
}

fn mode() -> Option<Mode> {
    let sre: u64;
    unsafe { asm!("mrs {}, ICC_SRE_EL1", out(reg) sre, options(nostack, nomem)) };
    if sre & 1 == 0 {
        return None;
    }
    let control: u64;
    let group: u64;
    unsafe {
        asm!("mrs {}, ICC_CTLR_EL1", "mrs {}, ICC_IGRPEN1_EL1", out(reg) control, out(reg) group, options(nostack, nomem))
    };
    (group & 1 != 0).then_some(if control & (1 << 1) == 0 {
        Mode::Combined
    } else {
        Mode::Split
    })
}

/// Observation after consulting the work owner and optionally waiting once.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Wake {
    Ready,
    Pending,
    Recheck,
}

/// One preallocated reception slot for this CPU Interface and its live frame.
#[must_use]
pub struct CpuInterface<'map> {
    frame: Frame<'map>,
    mode: Mode,
    state: State,
}

impl<'map> CpuInterface<'map> {
    /// # Safety
    /// The executing Non-secure PE owns the physical Group 1 interface and
    /// a non-nested, masked reception interval. Higher ELs grant system
    /// register access. Keep delivery mode, priority stack, isolation and
    /// Redistributor power stable until all claims settle. Source owners
    /// retain binding across IAR. Route only SGIs with active state here.
    pub unsafe fn admit(mut frame: Frame<'map>) -> Result<Self, (Frame<'map>, Failure)> {
        if !local(frame.affinity()) || !unsafe { available() } {
            return Err((frame, Failure::Scope));
        }
        if !frame.awake() {
            return Err((frame, Failure::Busy));
        }
        let Some(mode) = mode() else {
            return Err((frame, Failure::Scope));
        };
        Ok(Self {
            frame,
            mode,
            state: State::Empty,
        })
    }

    pub fn state(&self) -> State {
        self.state
    }

    /// Observe the original locally admitted destination of an SPI.
    pub fn destination(&self) -> Result<Destination, Failure> {
        self.context()?;
        Ok(Destination {
            domain: self.frame.domain(),
            frame: self.frame.physical(),
            affinity: self.frame.affinity(),
        })
    }

    /// Observe the exact route closed by its Distributor after all local
    /// acknowledge, priority drop and deactivation effects have settled.
    pub fn observe_retirement(
        &self,
        request: RetirementRequest,
    ) -> Result<RetirementReceipt, (RetirementRequest, Failure)> {
        let destination = match self.destination() {
            Ok(destination) => destination,
            Err(error) => return Err((request, error)),
        };
        if destination != request.destination() {
            return Err((request, Failure::Scope));
        }
        if self.state != State::Empty {
            return Err((request, Failure::Busy));
        }
        Ok(request.observed(self.frame.claim_round))
    }

    /// Return the original frame after all local receptions settle.
    pub fn release(self) -> Result<Frame<'map>, Self> {
        if self.state == State::Empty {
            Ok(self.frame)
        } else {
            Err(self)
        }
    }

    /// Open the original SGI for this live Group 1 interface.
    ///
    /// # Safety
    /// This source is reserved on the actual originating/receiving PEs.
    /// Exclude old or competing use and retain the destination and group
    /// configuration until the exact Sgi has completed.
    pub unsafe fn reserve_sgi(&mut self, intid: u8, priority: u8) -> Result<Sgi<'map>, Failure> {
        self.context()?;
        if self.state != State::Empty {
            return Err(Failure::Busy);
        }
        self.frame
            .prepare_signal(intid, priority)
            .map_err(|error| match error {
                super::super::redistributor::Failure::Busy => Failure::Busy,
                _ => Failure::Scope,
            })
    }

    /// Deliver once to the ready, independently owned destination PE.
    pub fn send_sgi(&mut self, signal: &mut Sgi<'_>) -> Result<(), Failure> {
        self.context()?;
        if self.frame.domain() != signal.domain || self.frame.physical() == signal.physical {
            return Err(Failure::Scope);
        }
        if signal.phase != Phase::Prepared {
            return Err(Failure::Busy);
        }
        signal.phase = Phase::Sending;
        unsafe {
            asm!("dsb ishst", "msr ICC_SGI1R_EL1, {}", "isb", in(reg) signal.address, options(nostack))
        };
        signal.phase = Phase::Sent;
        Ok(())
    }

    /// Consult the original work owner and wait once with the given admitted
    /// Group 1 SGI live. Recheck without reading IAR or dropping priority.
    /// The original owner retains pending delivery, even after a spurious WFI.
    ///
    /// # Safety
    /// WFI and wake are supported by the installed EL1 profile. The live SGI
    /// targets this Frame and its distributor routing stays enabled. Work
    /// retains a wake path and discoverable state across the final check;
    /// `ready` is bounded and retains no ordinary protection or owner borrow.
    pub unsafe fn wait_for_sgi(
        &mut self,
        id: u8,
        mut ready: impl FnMut() -> bool,
    ) -> Result<Wake, Failure> {
        self.context()?;
        if self.state != State::Empty || !can_switch() || !self.frame.awake() {
            return Err(Failure::Busy);
        }
        let priority = self.frame.signal_priority(id).ok_or(Failure::Scope)?;
        let (mask, running): (u64, u64);
        unsafe {
            asm!("mrs {}, ICC_PMR_EL1", "mrs {}, ICC_RPR_EL1", out(reg) mask, out(reg) running,
                 options(nostack, nomem, preserves_flags));
        }
        if u64::from(priority) >= (mask & 0xff) || running & 0xff != 0xff {
            return Err(Failure::Busy);
        }
        if ready() {
            return Ok(Wake::Ready);
        }
        if self.frame.signal_pending(id) {
            return Ok(Wake::Pending);
        }
        unsafe { wait_for_interrupt() };
        if ready() {
            Ok(Wake::Ready)
        } else if self.frame.signal_pending(id) {
            Ok(Wake::Pending)
        } else {
            Ok(Wake::Recheck)
        }
    }

    /// Prepare the exact PPI for a producer after the original input is quiet.
    /// The producer retains the PPI while this interface receives independently.
    pub fn prepare_ppi(&mut self, ppi: &mut Ppi<'_, '_>) -> Result<(), Failure> {
        self.context()?;
        if self.state != State::Empty {
            return Err(Failure::Busy);
        }
        self.frame
            .prepare_ppi(ppi)
            .map_err(|failure| match failure {
                redistributor::Failure::Busy => Failure::Busy,
                _ => Failure::Scope,
            })
    }

    fn context(&self) -> Result<(), Failure> {
        if !local(self.frame.affinity()) || mode() != Some(self.mode) {
            return Err(Failure::Scope);
        }
        Ok(())
    }

    /// Reborrow the exact outstanding claim after a temporary borrow ends.
    pub fn claim(&mut self) -> Option<Claim<'_, 'map>> {
        let id = self.state.claim()?;
        Some(Claim { owner: self, id })
    }

    pub fn receive(&mut self) -> Result<Option<Claim<'_, 'map>>, Failure> {
        self.context()?;
        let round = self
            .frame
            .claim_round
            .checked_add(1)
            .ok_or(Failure::Exhausted)?;
        self.state.begin_read()?;
        self.frame.claim_round = round;
        let raw: u64;
        unsafe { asm!("mrs {}, ICC_IAR1_EL1", "dsb sy", out(reg) raw, options(nostack)) };
        self.state.finish_read(raw)?;
        Ok(self.claim())
    }
}

/// Excludes another acknowledgement while this borrow can complete an EOI.
#[must_use]
pub struct Claim<'slot, 'map> {
    owner: &'slot mut CpuInterface<'map>,
    id: u32,
}

impl Claim<'_, '_> {
    pub fn intid(&self) -> u32 {
        self.id
    }

    pub(in crate::metal::interrupt::aarch64) fn matches_ppi(&self, ppi: &Ppi<'_, '_>) -> bool {
        ppi.matches(&self.owner.frame, self.id)
    }

    /// Settle this native receipt after its exact PPI producer has stopped.
    /// An unrelated claim leaves the stopped input available for retry.
    pub fn complete_ppi(&mut self, stopped: StoppedPpi<'_, '_, '_>) -> Result<(), Failure> {
        if !stopped.matches(&self.owner.frame, self.id) {
            return Err(Failure::Scope);
        }
        unsafe { crate::metal::interrupt::treaty::HardwareClaim::finish(self)? };
        stopped.complete();
        Ok(())
    }

    /// Complete precisely the SGI sent to this destination and retain a
    /// retirement witness in that original Sgi.
    pub fn complete_sgi(&mut self, signal: &mut Sgi<'_>) -> Result<(), Failure> {
        if signal.phase != Phase::Sent
            || self.owner.frame.physical() != signal.physical
            || self.owner.frame.domain() != signal.domain
            || self.id != u32::from(signal.id)
        {
            return Err(Failure::Scope);
        }
        if self.owner.state == State::Claimed(self.id) {
            // A sent SGI is an internally controlled edge with no separate
            // source-level mask or level deassertion obligation.
            unsafe { self.drop_priority()? };
        }
        if self.owner.state == State::DroppedPriority(self.id) {
            unsafe { self.deactivate()? };
        }
        if self.owner.state != State::Empty {
            return Err(Failure::Busy);
        }
        signal.phase = Phase::Settled;
        Ok(())
    }

    /// # Safety
    /// The original claim is durably associated or quarantined. The owner
    /// permits priority drop and, in combined mode, deactivation now.
    pub unsafe fn drop_priority(&mut self) -> Result<(), Failure> {
        self.owner.context()?;
        let id = self.id;
        self.owner.state.begin_priority_drop(id)?;
        unsafe {
            asm!("dsb sy", "msr ICC_EOIR1_EL1, {}", "isb", in(reg) u64::from(id), options(nostack))
        };
        self.owner.state.finish_priority_drop(id, self.owner.mode)
    }

    /// # Safety
    /// The original source is ready for deactivation. For level interrupts
    /// its assertion is cleared or its original route is durably masked.
    pub unsafe fn deactivate(&mut self) -> Result<(), Failure> {
        self.owner.context()?;
        let id = self.id;
        self.owner.state.begin_deactivation(id)?;
        unsafe {
            asm!("dsb sy", "msr ICC_DIR_EL1, {}", "isb", in(reg) u64::from(id), options(nostack))
        };
        self.owner.state.finish_deactivation(id)
    }
}

impl crate::metal::interrupt::treaty::claim::Sealed for Claim<'_, '_> {}

impl HardwareClaim for Claim<'_, '_> {
    type Error = Failure;

    fn identity(&self) -> ClaimId {
        ClaimId::new(
            self.owner.frame.physical().start(),
            self.owner.frame.affinity() as u64,
            self.owner.frame.claim_round,
            self.intid(),
        )
    }

    fn confirm(&mut self, evidence: ClaimConfirmation) -> Result<Completion, Failure> {
        if evidence.id() != self.identity() || evidence.accepted() {
            return Err(Failure::Scope);
        }
        self.owner.context()?;
        if self.owner.state != State::Empty {
            return Err(Failure::Busy);
        }
        Ok(Completion::Settled)
    }

    unsafe fn finish(&mut self) -> Result<Completion, Failure> {
        if self.owner.state == State::Claimed(self.id) {
            unsafe { self.drop_priority() }?;
            if self.owner.state == State::Empty {
                return Ok(Completion::Settled);
            }
        }
        unsafe { self.deactivate() }.map(|()| Completion::Settled)
    }
}

impl crate::metal::interrupt::treaty::claim::Sealed for CpuInterface<'_> {}

impl<'map> crate::metal::interrupt::treaty::Receive for CpuInterface<'map> {
    type Error = Failure;
    type Claim<'receiver>
        = Claim<'receiver, 'map>
    where
        Self: 'receiver;

    fn receive(&mut self) -> Result<Option<Self::Claim<'_>>, Failure> {
        CpuInterface::receive(self)
    }

    fn pending_claim(&mut self) -> Result<Option<Self::Claim<'_>>, Failure> {
        self.context()?;
        if matches!(
            self.state,
            State::Reading | State::DroppingPriority(_) | State::Deactivating(_)
        ) {
            return Err(Failure::Busy);
        }
        Ok(self.claim())
    }
}
