//! Exclusive supervisor timer programming through the actual SBI TIME service.

use super::Description;
use crate::metal::{
    chrono::riscv::RiscvTimebase,
    platform::firmware::sbi::{SbiServices, set_timer},
    processor::hart::local::current_hart,
};
use core::{arch::asm, marker::PhantomData};

const TIMER: usize = 1 << 5;
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum State {
    Idle,
    Armed(u64),
    Uncertain,
}

/// One actual Hart's exclusive timer programming responsibility.
/// Not Send or Sync: calls never program another Hart's comparison register.
#[must_use]
pub struct Timer<'a> {
    description: Description<'a>,
    timebase: RiscvTimebase<'a>,
    state: State,
    local: PhantomData<*mut ()>,
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
        timebase: RiscvTimebase<'a>,
        services: SbiServices,
    ) -> Result<Self, Failure> {
        if !services.timer() || description.clock_scope() != timebase.source() {
            return Err(Failure::Scope);
        }
        let timer = Self {
            description,
            timebase,
            state: State::Uncertain,
            local: PhantomData,
        };
        timer.context()?;
        Ok(timer)
    }

    pub fn state(&self) -> State {
        self.state
    }

    fn context(&self) -> Result<(), Failure> {
        let status: usize;
        unsafe { asm!("csrr {}, sstatus", out(reg) status, options(nostack, nomem)) };
        if current_hart().get() as u64 != self.description.hart() || status & 2 != 0 {
            return Err(Failure::Context);
        }
        Ok(())
    }

    /// Actual counter in the admitted domain, not a time-domain conversion.
    pub fn counter(&self) -> Result<u64, Failure> {
        self.context()?;
        Ok(unsafe { self.timebase.counter() })
    }

    pub fn pending(&self) -> Result<bool, Failure> {
        self.context()?;
        let pending: usize;
        unsafe { asm!("csrr {}, sip", out(reg) pending, options(nostack, nomem)) };
        Ok(pending & TIMER != 0)
    }

    /// Program an absolute future count. A previous comparison must have
    /// been settled; the caller owns the higher-level stop/wake requirements.
    pub fn arm(&mut self, count: u64) -> Result<(), Failure> {
        self.context()?;
        if self.state != State::Idle {
            return Err(Failure::Busy);
        }
        if count == u64::MAX || count <= self.counter()? {
            return Err(Failure::Range);
        }
        self.state = State::Uncertain;
        set_timer(count).map_err(Failure::Firmware)?;
        let enabled: usize;
        unsafe {
            asm!("csrs sie, {mask}", "csrr {enabled}, sie",
                mask = in(reg) TIMER, enabled = out(reg) enabled, options(nostack));
        }
        if enabled & TIMER == 0 {
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
        unsafe { asm!("csrc sie, {}", in(reg) TIMER, options(nostack)) };
        self.state = State::Uncertain;
        set_timer(u64::MAX).map_err(Failure::Firmware)?;
        for _ in 0..OBSERVATIONS {
            if !self.pending()? {
                self.state = State::Idle;
                return Ok(());
            }
            core::hint::spin_loop();
        }
        Err(Failure::Unconfirmed)
    }
}
