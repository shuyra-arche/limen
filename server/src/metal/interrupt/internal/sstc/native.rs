use super::{Failure, State};
use crate::metal::{
    chrono::riscv64::Counter,
    interrupt::internal::supervisor::{Cause, Reception},
    processor::{
        execution::description::riscv64::{RiscvExtension, RiscvIsa},
        hart::riscv64::description::RiscvCpu,
    },
};
use core::arch::asm;

fn compare() -> u64 {
    let count;
    unsafe { asm!("csrr {}, 0x14d", out(reg) count, options(nostack)) };
    count
}

fn set(count: u64) {
    unsafe { asm!("csrw 0x14d, {}", in(reg) count, options(nostack)) };
}

#[must_use]
pub struct Timer<'a> {
    counter: &'a Counter<'a>,
    local: &'a mut Reception,
    state: State,
}

impl<'a> Timer<'a> {
    /// # Safety
    /// The actual machine has granted S-mode time and stimecmp access
    /// (menvcfg.STCE, mcounteren.TM) and delegated STIP. The supplied
    /// ISA and counter refer to this running CPU. The caller exclusively
    /// owns the comparator, excludes SBI TIME and virtualization, and keeps
    /// the original owner through shutdown. Existing firmware deadlines have
    /// been transferred or settled before this owner changes them. A firmware name alone cannot
    /// establish these hardware and privilege facts.
    pub unsafe fn admit(
        cpu: &RiscvCpu<'_, '_>,
        isa: RiscvIsa<'_>,
        counter: &'a Counter<'a>,
        local: &'a mut Reception,
    ) -> Result<Self, Failure> {
        if !cpu.enabled()
            || cpu.id() != local.hart()
            || cpu.node().id() != isa.source()
            || cpu.node().parent_id() != Some(counter.source())
            || counter.hart() != cpu.id()
            || !isa.has(RiscvExtension::SupervisorTimer)
            || local.cause() != Cause::Timer
            || !local.valid()
        {
            return Err(Failure::Scope);
        }
        if local.enabled() {
            return Err(Failure::Busy);
        }
        Ok(Self {
            counter,
            local,
            state: State::Inherited(compare()),
        })
    }

    pub fn state(&self) -> State {
        self.state
    }
    fn context(&self) -> Result<(), Failure> {
        self.local.valid().then_some(()).ok_or(Failure::Scope)
    }

    /// Program or replace an absolute deadline in the admitted time domain.
    /// The caller preserves higher-level outstanding stop and wake obligations.
    pub fn program(&mut self, count: u64) -> Result<(), Failure> {
        self.context()?;
        if !matches!(self.state, State::Idle | State::Armed(_)) {
            return Err(Failure::Busy);
        }
        if count == u64::MAX || count <= self.counter.read().ok_or(Failure::Scope)? {
            return Err(Failure::Range);
        }
        self.state = State::Programming(count);
        set(count);
        if compare() != count {
            return Err(Failure::Unconfirmed);
        }
        self.local.set(true);
        if !self.local.enabled() {
            return Err(Failure::Unconfirmed);
        }
        self.state = State::Armed(count);
        Ok(())
    }

    /// A late STIP may have been produced by an earlier comparison.
    pub fn due(&self) -> Result<bool, Failure> {
        self.context()?;
        let State::Armed(count) = self.state else {
            return Err(Failure::Busy);
        };
        Ok(self.counter.read().ok_or(Failure::Scope)? >= count)
    }

    /// Mask delivery and replace the comparator; a delayed STIP requires
    /// another observation. This method never waits indefinitely for it.
    pub fn shut(&mut self) -> Result<(), Failure> {
        self.context()?;
        if self.state != State::Clearing {
            self.state = State::Programming(u64::MAX);
            self.local.set(false);
            if self.local.enabled() {
                return Err(Failure::Unconfirmed);
            }
            set(u64::MAX);
            if compare() != u64::MAX {
                return Err(Failure::Unconfirmed);
            }
            self.state = State::Clearing;
        }
        if self.local.pending() {
            return Err(Failure::Busy);
        }
        self.state = State::Idle;
        Ok(())
    }
}
