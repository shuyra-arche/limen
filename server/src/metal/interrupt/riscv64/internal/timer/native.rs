use super::{ClearStep, Clearing, Effect, Failure, Prior, ProgramStep, Programming, State};
#[cfg(limen_firmware = "sbi")]
use crate::metal::platform::firmware::sbi::TimerService;
use crate::metal::{
    chrono::riscv64::Counter,
    interrupt::riscv64::internal::supervisor::{Cause, Reception},
    processor::{
        execution::description::riscv64::{RiscvExtension, RiscvIsa},
        hart::riscv64::description::RiscvCpu,
    },
};
use core::arch::asm;

const OBSERVATIONS: usize = 1024;

#[derive(Clone, Copy)]
enum Comparator {
    Sstc,
    #[cfg(limen_firmware = "sbi")]
    Sbi(TimerService),
}

impl Comparator {
    fn inherited(self) -> Option<u64> {
        match self {
            Self::Sstc => Some(compare()),
            #[cfg(limen_firmware = "sbi")]
            Self::Sbi(_) => None,
        }
    }

    fn set(self, count: u64) -> Result<(), Failure> {
        match self {
            Self::Sstc => {
                unsafe { asm!("csrw 0x14d, {}", in(reg) count, options(nostack)) };
                (compare() == count)
                    .then_some(())
                    .ok_or(Failure::Unconfirmed)
            }
            #[cfg(limen_firmware = "sbi")]
            Self::Sbi(service) => service.set(count).map_err(Failure::Firmware),
        }
    }
}

fn compare() -> u64 {
    let count;
    unsafe { asm!("csrr {}, 0x14d", out(reg) count, options(nostack)) };
    count
}

/// One actual Hart's exclusive timer programming responsibility.
/// Not Send or Sync: calls never program another Hart's comparison register.
#[must_use]
pub struct Timer<'a> {
    counter: &'a Counter<'a>,
    local: &'a mut Reception,
    comparator: Comparator,
    state: State,
}

impl<'a> Timer<'a> {
    /// # Safety
    /// The actual Hart grants stimecmp and time access to this supervisor
    /// and delegates STIP. The original comparison and wake obligations
    /// have been settled or transferred. No other programmer may access
    /// the same comparator until this owner shuts successfully.
    pub unsafe fn admit_sstc(
        cpu: &RiscvCpu<'_, '_>,
        isa: RiscvIsa<'_>,
        counter: &'a Counter<'a>,
        local: &'a mut Reception,
    ) -> Result<Self, Failure> {
        if !cpu.enabled()
            || cpu.node().id() != isa.source()
            || cpu.node().parent_id() != Some(counter.source())
            || counter.hart() != cpu.id()
            || !isa.has(RiscvExtension::SupervisorTimer)
        {
            return Err(Failure::Scope);
        }
        Self::admit(counter, local, Comparator::Sstc)
    }

    /// # Safety
    /// The actual SBI TIME service controls this Hart's comparator in the
    /// admitted time domain. Its original comparison and wake obligations
    /// have been settled or transferred. No other programmer may access
    /// the same comparator until this owner shuts successfully.
    #[cfg(limen_firmware = "sbi")]
    pub unsafe fn admit_sbi(
        counter: &'a Counter<'a>,
        service: TimerService,
        local: &'a mut Reception,
    ) -> Result<Self, Failure> {
        Self::admit(counter, local, Comparator::Sbi(service))
    }

    fn admit(
        counter: &'a Counter<'a>,
        local: &'a mut Reception,
        comparator: Comparator,
    ) -> Result<Self, Failure> {
        if counter.hart() != local.hart() || local.cause() != Cause::Timer {
            return Err(Failure::Scope);
        }
        if !local.valid() {
            return Err(Failure::Context);
        }
        if local.enabled() {
            return Err(Failure::Busy);
        }
        Ok(Self {
            counter,
            local,
            comparator,
            state: State::Inherited(comparator.inherited()),
        })
    }

    pub fn state(&self) -> State {
        self.state
    }

    fn context(&self) -> Result<(), Failure> {
        if !self.local.valid() || self.local.hart() != self.counter.hart() {
            return Err(Failure::Context);
        }
        Ok(())
    }

    /// Actual counter in the admitted domain, not a time-domain conversion.
    pub fn counter(&self) -> Result<u64, Failure> {
        self.context()?;
        self.counter.read().ok_or(Failure::Context)
    }

    pub fn pending(&self) -> Result<bool, Failure> {
        self.context()?;
        Ok(self.local.pending())
    }

    /// Program an absolute future count. A previous comparison must have
    /// been settled; the caller owns the higher-level stop/wake requirements.
    pub fn arm(&mut self, count: u64) -> Result<(), Failure> {
        self.context()?;
        if self.state != State::Idle {
            return Err(Failure::Busy);
        }
        self.program(None, count)
    }

    /// Replace the current comparison without first masking or cancelling it.
    /// The caller must preserve every outstanding stop/wake requirement.
    pub fn rearm(&mut self, count: u64) -> Result<(), Failure> {
        self.context()?;
        let State::Armed(previous) = self.state else {
            return Err(Failure::Busy);
        };
        self.program(Some(previous), count)
    }

    fn program(&mut self, previous: Option<u64>, count: u64) -> Result<(), Failure> {
        if count == u64::MAX || count <= self.counter()? {
            return Err(Failure::Range);
        }
        let mut program = Programming {
            previous,
            requested: count,
            step: ProgramStep::Compare,
        };
        self.state = State::Programming(program);
        if let Err(error) = self.comparator.set(count) {
            self.state = State::Uncertain(Effect::Programming(program));
            return Err(error);
        }
        program.step = ProgramStep::Enable;
        self.state = State::Programming(program);
        self.local.set(true);
        if !self.local.enabled() {
            self.state = State::Uncertain(Effect::Programming(program));
            return Err(Failure::Unconfirmed);
        }
        self.state = State::Armed(count);
        Ok(())
    }

    /// A received IRQ is only an opportunity to inspect the original count.
    /// A stale notification must not prove expiration.
    pub fn due(&self) -> Result<bool, Failure> {
        self.context()?;
        let deadline = self.state.deadline().ok_or(Failure::Busy)?;
        Ok(self.counter()? >= deadline)
    }

    /// Mask the local interrupt and move the comparison to the far future.
    /// Failure preserves the original responsibility in this object.
    /// The caller must already have settled or transferred higher-level
    /// stop/wake obligations. This does not shut down the counter itself.
    pub fn shut(&mut self) -> Result<(), Failure> {
        self.context()?;
        let prior = match self.state {
            State::Inherited(count) => Prior::Inherited(count),
            State::Idle => Prior::Idle,
            State::Armed(count) => Prior::Armed(count),
            State::Uncertain(Effect::Programming(program)) => Prior::Programming(program),
            State::Uncertain(Effect::Clearing(clear)) => clear.prior,
            State::AwaitingClear(_) => return self.observe_clear(),
            // An actual operation must return to this exclusive owner first.
            State::Programming(_) | State::Clearing(_) => return Err(Failure::Busy),
        };
        let mut clear = Clearing {
            prior,
            step: ClearStep::Mask,
        };
        self.state = State::Clearing(clear);
        self.local.set(false);
        if self.local.enabled() {
            self.state = State::Uncertain(Effect::Clearing(clear));
            return Err(Failure::Unconfirmed);
        }
        clear.step = ClearStep::Compare;
        self.state = State::Clearing(clear);
        // Once a replacement is confirmed, it covers either previous
        // value, including an uncertain program or inherited deadline.
        if let Err(error) = self.comparator.set(u64::MAX) {
            self.state = State::Uncertain(Effect::Clearing(clear));
            return Err(error);
        }
        self.state = State::AwaitingClear(prior);
        self.observe_clear()
    }

    fn observe_clear(&mut self) -> Result<(), Failure> {
        // A retry after a confirmed comparison only observes, never reissues the write.
        if self.local.enabled() {
            self.local.set(false);
            if self.local.enabled() {
                return Err(Failure::Unconfirmed);
            }
        }
        for _ in 0..OBSERVATIONS {
            if !self.pending()? {
                self.state = State::Idle;
                return Ok(());
            }
            core::hint::spin_loop();
        }
        // The write was confirmed. Preserve that fact even if the observation
        // allowance is exhausted; the next shut continues at this boundary.
        Err(Failure::Busy)
    }
}
