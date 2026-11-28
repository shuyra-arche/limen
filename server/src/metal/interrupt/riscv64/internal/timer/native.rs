use super::{ClearStep, Clearing, Effect, Event, Failure, Prior, ProgramStep, State};
#[cfg(limen_firmware = "sbi")]
use crate::metal::platform::firmware::treaty::sbi::TimerService;
use crate::metal::{
    chrono::{riscv64::time::Counter, treaty::Deadline},
    interrupt::{
        riscv64::internal::supervisor::{Cause, Reception},
        treaty::{AbsoluteTimer, TimerEffect, absolute::Sealed},
    },
    processor::{
        description::riscv64::{
            cpu::RiscvCpu,
            isa::{RiscvExtension, RiscvIsa},
        },
        treaty::TimerInterrupt,
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
    fn read_comparison(self) -> Option<u64> {
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
            state: State::Inherited(comparator.read_comparison()),
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
        let target = self
            .counter
            .clock()
            .count_for(deadline)
            .ok_or(Failure::Scope)?;
        let now = self.counter()?;
        self.state.advance(Event::Rearm { now, target })?;
        self.drive()
    }

    /// Record the due original comparison and stop its interrupt delivery.
    /// An early or stale pending hint leaves the confirmed comparison live.
    /// Ordinary execution settles the retained comparison via `stop_due`.
    ///
    /// # Safety
    /// The caller can make ordinary progress on this owner after IRQ return.
    /// Masking this line preserves any independent stop or wake obligations.
    pub unsafe fn accept_interrupt(
        &mut self,
        interrupt: &TimerInterrupt<'_>,
    ) -> Result<bool, Failure> {
        if interrupt.processor().get() as u64 != self.local.hart() || !self.local.enabled() {
            return Err(Failure::Context);
        }
        let target = self.state.deadline().ok_or(Failure::Busy)?;
        let now = self
            .counter
            .clock()
            .read_on_interrupt(interrupt)
            .ok_or(Failure::Context)?;
        if now < target || !self.local.pending() {
            return Ok(false);
        }
        self.state.advance(Event::Shut)?;
        self.local.set(false);
        self.state.advance(Event::Masked(!self.local.enabled()))?;
        interrupt.confirm_receipt();
        Ok(true)
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

    fn observe_clear(&self) -> Result<bool, Failure> {
        if self.local.enabled() {
            return Err(Failure::Unconfirmed);
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
        let target = self
            .counter
            .clock()
            .count_for(deadline)
            .ok_or(Failure::Scope)?;
        let now = self.counter()?;
        self.state.advance(Event::Arm { now, target })?;
        self.drive()
    }

    fn advance_deadline(&mut self, deadline: Deadline<'_>) -> Result<(), Failure> {
        let target = self
            .counter
            .clock()
            .count_for(deadline)
            .ok_or(Failure::Scope)?;
        let previous = self.state.deadline().ok_or(Failure::Busy)?;
        let now = self.counter()?;
        if now >= previous || target > previous {
            return Err(Failure::Busy);
        }
        self.state.advance(Event::Rearm { now, target })?;
        self.drive()
    }

    fn confirmed_deadline(&self) -> Result<Option<Deadline<'_>>, Failure> {
        self.context()?;
        match self.state {
            State::Idle => Ok(None),
            State::Armed(target) => Ok(Some(self.counter.deadline(target))),
            _ => Err(Failure::Busy),
        }
    }

    fn due(&self) -> Result<bool, Failure> {
        self.context()?;
        let deadline = self.state.deadline().ok_or(Failure::Busy)?;
        Ok(self.counter()? >= deadline)
    }

    fn pending_effect(&self) -> Result<Option<TimerEffect>, Failure> {
        self.context()?;
        Ok(match self.state {
            State::Programming(program) | State::Uncertain(Effect::Programming(program)) => {
                Some(TimerEffect::Programming {
                    previous: program.previous,
                    requested: program.requested,
                })
            }
            State::Clearing(_)
            | State::AwaitingClear(_)
            | State::Uncertain(Effect::Clearing(_)) => Some(TimerEffect::Clearing),
            State::Inherited(_) | State::Idle | State::Armed(_) => None,
        })
    }

    fn confirm_effect(&mut self) -> Result<(), Failure> {
        self.context()?;
        let event = match self.state {
            State::Uncertain(Effect::Programming(program)) => match program.step {
                ProgramStep::Compare => {
                    let observed = self.comparator.read_comparison();
                    Event::Compared(
                        (observed == Some(program.requested))
                            .then_some(())
                            .ok_or(Failure::Unconfirmed),
                    )
                }
                ProgramStep::Enable => Event::Enabled(self.local.enabled()),
            },
            State::Uncertain(Effect::Clearing(clear)) => match clear.step {
                ClearStep::Mask => Event::Masked(!self.local.enabled()),
                ClearStep::Compare => Event::Compared(
                    (self.comparator.read_comparison() == Some(u64::MAX))
                        .then_some(())
                        .ok_or(Failure::Unconfirmed),
                ),
            },
            State::Clearing(_) | State::AwaitingClear(_) => return self.drive(),
            _ => return Err(Failure::Busy),
        };
        self.state.advance(event)?;
        self.drive()
    }

    unsafe fn clear(&mut self) -> Result<(), Failure> {
        self.context()?;
        if self.pending_effect()? == Some(TimerEffect::Clearing) {
            return self.confirm_effect();
        }
        if self.state == State::Idle {
            return (!self.local.enabled() && !self.local.pending())
                .then_some(())
                .ok_or(Failure::Unconfirmed);
        }
        self.state.advance(Event::Shut)?;
        self.drive()
    }

    unsafe fn stop_due(&mut self) -> Result<(), Failure> {
        let deadline = match self.state {
            State::Armed(target) => target,
            State::Clearing(Clearing {
                prior: Prior::Armed(_),
                ..
            })
            | State::AwaitingClear(Prior::Armed(_)) => return self.confirm_effect(),
            State::Uncertain(Effect::Clearing(clear)) if matches!(clear.prior, Prior::Armed(_)) => {
                return self.confirm_effect();
            }
            _ => return Err(Failure::Busy),
        };
        if self.counter()? < deadline {
            return Err(Failure::Busy);
        }
        unsafe { self.clear() }
    }
}
