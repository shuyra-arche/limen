//! Programming of the original PE's virtual comparator and timer PPI.

use core::arch::asm;

use super::{Failure, GenericTimer, State};
use crate::metal::{
    chrono::{Clock, Deadline},
    interrupt::{
        AbsoluteTimer,
        aarch64::gic::{cpu_interface::CpuInterface, redistributor::Ppi},
        absolute::Sealed,
    },
};

const ENABLE: u64 = 1;
const MASK: u64 = 2;
const PENDING: u64 = 4;

/// Exclusive programming of one actual virtual comparator.
#[must_use]
pub struct VirtualTimer<'scope, 'tree, 'map> {
    clock: &'scope Clock<'tree>,
    ppi: &'scope Ppi<'tree>,
    receiver: &'scope mut CpuInterface<'map>,
    state: State,
}

impl<'scope, 'tree, 'map> VirtualTimer<'scope, 'tree, 'map> {
    /// # Safety
    /// The local virtual comparator and the actual PPI belong to this
    /// participant. EL1 has virtual counter and comparator access. Retain
    /// the original clock offset and source association while admitted.
    /// The preceding comparator has no outstanding stop or wake promise.
    pub unsafe fn admit(
        description: GenericTimer<'tree>,
        clock: &'scope Clock<'tree>,
        ppi: &'scope Ppi<'tree>,
        receiver: &'scope mut CpuInterface<'map>,
    ) -> Result<Self, Failure> {
        if clock.source() != description.source()
            || ppi.source() != description.source()
            || ppi.domain() != description.domain()
            || ppi.intid() != description.virtual_interrupt().intid
            || clock.read().is_none()
            || !receiver.admits_ppi(ppi)
        {
            return Err(Failure::Scope);
        }
        if control() & ENABLE != 0 {
            return Err(Failure::Busy);
        }
        Ok(Self {
            clock,
            ppi,
            receiver,
            state: State::Idle,
        })
    }

    pub fn state(&self) -> State {
        self.state
    }

    /// Reborrow this comparator's original receiver for native PPI claims.
    pub fn receiver(&mut self) -> &mut CpuInterface<'map> {
        self.receiver
    }

    pub fn rearm_at(&mut self, deadline: Deadline<'_, '_>) -> Result<(), Failure> {
        let State::Armed(previous) = self.state else {
            return Err(Failure::Busy);
        };
        self.program(Some(previous), deadline)
    }

    fn program(
        &mut self,
        previous: Option<u64>,
        deadline: Deadline<'_, '_>,
    ) -> Result<(), Failure> {
        if !self.receiver.admits_ppi(self.ppi) {
            return Err(Failure::Scope);
        }
        let target = deadline.count_for(self.clock).ok_or(Failure::Scope)?;
        let now = self.clock.read().ok_or(Failure::Scope)?;
        self.state.begin_program(previous, now, target)?;
        unsafe {
            asm!(
                "msr cntv_cval_el0, {target}",
                "msr cntv_ctl_el0, {enable}",
                "isb",
                target = in(reg) target,
                enable = in(reg) ENABLE,
                options(nostack, preserves_flags)
            )
        };
        self.confirm_program()
    }

    /// Observe the exact outstanding CVAL and enable; no unrelated request
    /// can overwrite either comparison while its outcome is unknown.
    pub fn confirm_program(&mut self) -> Result<(), Failure> {
        if self.clock.read().is_none() {
            return Err(Failure::Scope);
        }
        let State::Programming { .. } = self.state else {
            return Err(Failure::Busy);
        };
        let compare: u64;
        unsafe { asm!("mrs {}, cntv_cval_el0", out(reg) compare, options(nostack, nomem)) };
        self.state
            .finish_program(compare, control() & (ENABLE | MASK) == ENABLE)
    }

    pub fn pending(&self) -> Result<bool, Failure> {
        Ok(self.due()? && control() & (ENABLE | MASK | PENDING) == (ENABLE | PENDING))
    }

    /// Disable the expired original comparator. The GIC PPI remains owed
    /// until the separate original claim is completed on its CPU Interface.
    pub fn stop(&mut self) -> Result<(), Failure> {
        let now = self.clock.read().ok_or(Failure::Scope)?;
        self.state.begin_stop(now)?;
        unsafe {
            asm!("msr cntv_ctl_el0, {}", "isb", in(reg) MASK, options(nostack, preserves_flags))
        };
        self.state.finish_stop(control() & (ENABLE | MASK) == MASK)
    }
}

impl Sealed for VirtualTimer<'_, '_, '_> {}

impl AbsoluteTimer for VirtualTimer<'_, '_, '_> {
    type Failure = Failure;

    fn arm_at(&mut self, target: Deadline<'_, '_>) -> Result<(), Failure> {
        if self.state != State::Idle {
            return Err(Failure::Busy);
        }
        self.program(None, target)
    }

    fn due(&self) -> Result<bool, Failure> {
        let now = self.clock.read().ok_or(Failure::Scope)?;
        let State::Armed(target) = self.state else {
            return Err(Failure::Busy);
        };
        Ok(now >= target)
    }
}

fn control() -> u64 {
    let bits;
    unsafe { asm!("mrs {}, cntv_ctl_el0", out(reg) bits, options(nostack, nomem)) };
    bits
}
