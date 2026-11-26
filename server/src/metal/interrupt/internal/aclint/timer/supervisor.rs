//! Exclusive supervisor timer programming through the actual SBI TIME service.

use super::Description;
use crate::metal::{
    chrono::riscv::Counter,
    interrupt::internal::supervisor::{Cause, Reception},
    platform::firmware::sbi::TimerService,
};
const OBSERVATIONS: usize = 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Failure {
    Scope,
    Context,
    Busy,
    Range,
    Firmware(isize),
    Unconfirmed,
}

/// The comparison write has not been confirmed, or the comparison is
/// confirmed but the local timer enable is still owed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProgramStep {
    Compare,
    Enable,
}

/// One synchronous replacement of the actual comparator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Programming {
    pub previous: Option<u64>,
    pub requested: u64,
    pub step: ProgramStep,
}

/// The original comparison facts that a clearing operation must retain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Prior {
    Inherited,
    Idle,
    Armed(u64),
    Programming(Programming),
}

/// The stage of the original clear whose effect is still owed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClearStep {
    Mask,
    Compare,
    Observe,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Clearing {
    pub prior: Prior,
    pub step: ClearStep,
}

/// The specific operation that returned without confirmation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Effect {
    Programming(Programming),
    Clearing(Clearing),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum State {
    Inherited,
    Idle,
    Programming(Programming),
    Armed(u64),
    Clearing(Clearing),
    AwaitingClear(Prior),
    Uncertain(Effect),
}

/// One actual Hart's exclusive timer programming responsibility.
/// Not Send or Sync: calls never program another Hart's comparison register.
#[must_use]
pub struct Timer<'a> {
    description: Description<'a>,
    counter: &'a Counter<'a>,
    local: &'a mut Reception,
    service: TimerService,
    state: State,
}

impl<'a> Timer<'a> {
    /// # Safety
    /// The trusted platform establishes that SBI TIME uses this Hart's
    /// described ACLINT counter, grants time access, and excludes every other
    /// timer programmer (including a direct Sstc path). All original stop and
    /// wake obligations have been transferred or settled. Retain this owner
    /// and its obligations until shut succeeds; dropping it does not retire
    /// the hardware. Do not enable global interrupts before a trusted entry
    /// can handle the supervisor timer interrupt.
    pub unsafe fn admit(
        description: Description<'a>,
        counter: &'a Counter<'a>,
        service: TimerService,
        local: &'a mut Reception,
    ) -> Result<Self, Failure> {
        if description.clock_scope() != counter.source()
            || description.hart() != counter.hart()
            || description.hart() != local.hart()
            || local.cause() != Cause::Timer
        {
            return Err(Failure::Scope);
        }
        let timer = Self {
            description,
            counter,
            local,
            service,
            state: State::Inherited,
        };
        timer.context()?;
        Ok(timer)
    }

    pub fn state(&self) -> State {
        self.state
    }

    fn context(&self) -> Result<(), Failure> {
        if !self.local.valid() || self.local.hart() != self.description.hart() {
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
        if let Err(error) = self.service.set(count) {
            self.state = State::Uncertain(Effect::Programming(program));
            return Err(Failure::Firmware(error));
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
        match self.state {
            State::Armed(count) => Ok(self.counter()? >= count),
            _ => Err(Failure::Busy),
        }
    }

    /// Mask the local interrupt and move the comparison to the far future.
    /// Failure preserves the original responsibility in this object.
    /// The caller must already have settled or transferred higher-level
    /// stop/wake obligations. This does not shut down the counter itself.
    pub fn shut(&mut self) -> Result<(), Failure> {
        self.context()?;
        let prior = match self.state {
            State::Inherited => Prior::Inherited,
            State::Idle => Prior::Idle,
            State::Armed(count) => Prior::Armed(count),
            State::Uncertain(Effect::Programming(program)) => Prior::Programming(program),
            State::Uncertain(Effect::Clearing(clear)) => clear.prior,
            State::AwaitingClear(prior) => return self.observe_clear(prior),
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
        // SBI TIME is synchronous. Once the original call has returned,
        // this confirmed replacement covers the previous possible values.
        if let Err(error) = self.service.set(u64::MAX) {
            self.state = State::Uncertain(Effect::Clearing(clear));
            return Err(Failure::Firmware(error));
        }
        self.state = State::AwaitingClear(prior);
        self.observe_clear(prior)
    }

    fn observe_clear(&mut self, prior: Prior) -> Result<(), Failure> {
        // A retry after confirmed TIME only observes, never reissues the write.
        if self.local.enabled() {
            self.state = State::Uncertain(Effect::Clearing(Clearing {
                prior,
                step: ClearStep::Observe,
            }));
            return Err(Failure::Unconfirmed);
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
        Err(Failure::Unconfirmed)
    }
}
