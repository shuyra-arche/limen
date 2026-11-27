use super::{ClearStep, Effect, Event, Failure, Prior, ProgramStep, State};
#[cfg(limen_firmware = "sbi")]
use crate::metal::platform::firmware::sbi::TimerService;
use crate::metal::{
    chrono::{Deadline, riscv64::Counter},
    interrupt::{
        AbsoluteTimer,
        absolute::Sealed,
        riscv64::internal::supervisor::{Cause, Reception},
    },
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

    /// Replace the current comparison without first masking it.
    ///
    /// # Safety
    /// This replacement preserves every committed stop and wake obligation.
    /// A later target requires independent evidence that earlier obligations
    /// have settled or another source still covers them.
    pub unsafe fn rearm(&mut self, deadline: Deadline<'_>) -> Result<(), Failure> {
        let target = deadline
            .count_for(self.counter.clock())
            .ok_or(Failure::Scope)?;
        let now = self.counter()?;
        self.state.advance(Event::Rearm { now, target })?;
        self.drive()
    }

    /// Mask the interrupt and move the comparison to the far future.
    /// Failure preserves the original comparator responsibility.
    ///
    /// # Safety
    /// All higher-level stop and wake obligations have settled or are
    /// continuously covered elsewhere throughout this operation.
    pub unsafe fn shut(&mut self) -> Result<(), Failure> {
        self.context()?;
        self.state.advance(Event::Shut)?;
        self.drive()
    }

    fn drive(&mut self) -> Result<(), Failure> {
        loop {
            let event = match self.state {
                State::Programming(program) => match program.step {
                    ProgramStep::Compare => Event::Compared(self.comparator.set(program.requested)),
                    ProgramStep::Enable => {
                        self.local.set(true);
                        Event::Enabled(self.local.enabled())
                    }
                },
                State::Clearing(clear) => match clear.step {
                    ClearStep::Mask => {
                        self.local.set(false);
                        Event::Masked(!self.local.enabled())
                    }
                    ClearStep::Compare => Event::Compared(self.comparator.set(u64::MAX)),
                },
                State::AwaitingClear(_) => Event::Drained(self.observe_clear()?),
                _ => return Ok(()),
            };
            self.state.advance(event)?;
        }
    }

    fn observe_clear(&mut self) -> Result<bool, Failure> {
        if self.local.enabled() {
            self.local.set(false);
            if self.local.enabled() {
                return Err(Failure::Unconfirmed);
            }
        }
        for _ in 0..OBSERVATIONS {
            if !self.pending()? {
                return Ok(true);
            }
            core::hint::spin_loop();
        }
        // The comparator write was confirmed. A retry only observes drain.
        Ok(false)
    }
}

impl Sealed for Timer<'_> {}

impl AbsoluteTimer for Timer<'_> {
    type Failure = Failure;

    fn arm_at(&mut self, deadline: Deadline<'_>) -> Result<(), Failure> {
        let target = deadline
            .count_for(self.counter.clock())
            .ok_or(Failure::Scope)?;
        let now = self.counter()?;
        self.state.advance(Event::Arm { now, target })?;
        self.drive()
    }

    fn advance_deadline(&mut self, deadline: Deadline<'_>) -> Result<(), Failure> {
        let target = deadline
            .count_for(self.counter.clock())
            .ok_or(Failure::Scope)?;
        let previous = self.state.deadline().ok_or(Failure::Busy)?;
        let now = self.counter()?;
        if now >= previous || target > previous {
            return Err(Failure::Busy);
        }
        self.state.advance(Event::Rearm { now, target })?;
        self.drive()
    }

    fn due(&self) -> Result<bool, Failure> {
        self.context()?;
        let deadline = self.state.deadline().ok_or(Failure::Busy)?;
        Ok(self.counter()? >= deadline)
    }

    unsafe fn stop_due(&mut self) -> Result<(), Failure> {
        let deadline = match self.state {
            State::Armed(target) => target,
            State::AwaitingClear(Prior::Armed(_)) => return unsafe { self.shut() },
            State::Uncertain(Effect::Clearing(clear)) if matches!(clear.prior, Prior::Armed(_)) => {
                return unsafe { self.shut() };
            }
            _ => return Err(Failure::Busy),
        };
        if self.counter()? < deadline {
            return Err(Failure::Busy);
        }
        unsafe { self.shut() }
    }
}
