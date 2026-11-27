//! The local xAPIC one-shot countdown and its original receiving vector.

use core::num::NonZeroU32;

use super::{Failure, LocalApic, VectorLease};

const LVT: usize = 0x320;
const INITIAL: usize = 0x380;
const CURRENT: usize = 0x390;
const DIVIDE: usize = 0x3e0;
const MASKED: u32 = 1 << 16;
const DELIVERY_PENDING: u32 = 1 << 12;
const TIMER_MODE: u32 = 3 << 17;
const PERIODIC: u32 = 1 << 17;
const LVT_FIELDS: u32 = MASKED | TIMER_MODE | 0xff;
const DIVIDE_FIELDS: u32 = (1 << 3) | 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Divider {
    One,
    Two,
    Four,
    Eight,
    Sixteen,
    ThirtyTwo,
    SixtyFour,
    OneHundredTwentyEight,
}

impl Divider {
    const fn bits(self) -> u32 {
        match self {
            Self::One => 0b1011,
            Self::Two => 0b0000,
            Self::Four => 0b0001,
            Self::Eight => 0b0010,
            Self::Sixteen => 0b0011,
            Self::ThirtyTwo => 0b1000,
            Self::SixtyFour => 0b1001,
            Self::OneHundredTwentyEight => 0b1010,
        }
    }
}

/// A relative duration in ticks of this APIC's divided timer clock.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Countdown {
    pub ticks: NonZeroU32,
    pub divider: Divider,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimerState {
    Reserved,
    /// Readback has not confirmed the original programming.
    Arming(Countdown),
    /// The countdown was programmed. Its interrupt may already be pending.
    Armed(Countdown),
    /// The stop operation still owes readback for this exact countdown.
    Stopping(Countdown),
    /// Future expirations are stopped. Any original IRQ still needs service.
    Stopped,
}

impl TimerState {
    fn begin_arm(self, countdown: Countdown) -> Result<Self, Failure> {
        if self != Self::Reserved {
            return Err(Failure::Busy);
        }
        Ok(Self::Arming(countdown))
    }

    fn begin_stop(self) -> Self {
        match self {
            Self::Arming(countdown) | Self::Armed(countdown) => Self::Stopping(countdown),
            Self::Reserved | Self::Stopping(_) | Self::Stopped => self,
        }
    }
}

pub(super) struct Timer {
    vector: VectorLease,
    state: TimerState,
}

#[derive(Clone, Copy)]
pub(super) struct Inherited {
    lvt: u32,
}

impl Inherited {
    fn observe(lvt: u32) -> Result<Self, Failure> {
        if lvt & TIMER_MODE > PERIODIC {
            return Err(Failure::Scope);
        }
        Ok(Self {
            lvt: lvt & LVT_FIELDS,
        })
    }

    fn vector(self) -> u8 {
        self.lvt as u8
    }

    fn masked(self) -> u32 {
        self.lvt | MASKED
    }
}

pub(super) enum Slot {
    Unobserved,
    Retiring(Inherited),
    Vacant,
    Bound(Timer),
}

impl Slot {
    fn bound(&self) -> Result<&Timer, Failure> {
        match self {
            Self::Bound(timer) => Ok(timer),
            _ => Err(Failure::Scope),
        }
    }

    fn bound_mut(&mut self) -> Result<&mut Timer, Failure> {
        match self {
            Self::Bound(timer) => Ok(timer),
            _ => Err(Failure::Scope),
        }
    }
}

impl LocalApic<'_> {
    pub fn timer_state(&self) -> Option<TimerState> {
        self.timer.bound().ok().map(|timer| timer.state)
    }

    /// Quiesce the exact inherited legacy countdown before admitting a new one.
    ///
    /// # Safety
    /// The caller owns the inherited timer from the original firmware
    /// handoff. The old vector's handler and its original obligations remain
    /// available until native receive and in-service state has drained.
    /// Any previous software reception must be settled by its original owner.
    pub unsafe fn retire_inherited_timer(&mut self) -> Result<(), Failure> {
        self.reception_context()?;
        let original = match self.timer {
            Slot::Unobserved => Inherited::observe(self.timer_read(LVT))?,
            Slot::Retiring(original) => original,
            Slot::Vacant => return Ok(()),
            Slot::Bound(_) => return Err(Failure::Busy),
        };
        self.timer = Slot::Retiring(original);
        let masked = original.masked();
        self.timer_write(LVT, masked);
        self.timer_write(INITIAL, 0);
        if !self.timer_matches(LVT, LVT_FIELDS, masked) || !self.timer_quiet() {
            return Err(Failure::Unconfirmed);
        }
        let vector = original.vector();
        if vector >= 32 && self.vector_busy(vector) {
            return Err(Failure::Busy);
        }
        self.timer = Slot::Vacant;
        Ok(())
    }

    /// Transfer an original, already reserved vector to this idle timer.
    /// Failure returns the lease before effects can escape.
    pub fn reserve_timer(&mut self, vector: VectorLease) -> Result<(), (VectorLease, Failure)> {
        if self.reception_context().is_err()
            || !vector.matches(self.processor as u8, vector.vector())
        {
            return Err((vector, Failure::Scope));
        }
        if !matches!(self.timer, Slot::Vacant)
            || !self.timer_quiet()
            || self.vector_busy(vector.vector())
        {
            return Err((vector, Failure::Busy));
        }
        self.timer = Slot::Bound(Timer {
            vector,
            state: TimerState::Reserved,
        });
        Ok(())
    }

    /// Start a one-shot countdown on the owning processor.
    ///
    /// # Safety
    /// The caller owns the original vector's handling path. Interrupts
    /// remain masked until that path is ready, and the caller retains it for
    /// any delivery that has not completed until the vector is released.
    pub unsafe fn arm_timer(&mut self, countdown: Countdown) -> Result<(), Failure> {
        self.reception_context()?;
        let timer = self.timer.bound_mut()?;
        timer.state = timer.state.begin_arm(countdown)?;
        let configuration = u32::from(timer.vector.vector());
        self.timer_write(DIVIDE, countdown.divider.bits());
        self.timer_write(LVT, configuration);
        self.timer_write(INITIAL, countdown.ticks.get());
        if !self.timer_matches(LVT, LVT_FIELDS, configuration)
            || !self.timer_matches(DIVIDE, DIVIDE_FIELDS, countdown.divider.bits())
            || self.timer_read(INITIAL) != countdown.ticks.get()
        {
            return Err(Failure::Unconfirmed);
        }
        self.timer
            .bound_mut()
            .expect("timer remains reserved")
            .state = TimerState::Armed(countdown);
        Ok(())
    }

    /// Mask the original timer and clear its counter. Keep its vector leased
    /// until any pending or in-service interrupt has been completed.
    pub fn stop_timer(&mut self) -> Result<(), Failure> {
        self.reception_context()?;
        let timer = self.timer.bound_mut()?;
        timer.state = timer.state.begin_stop();
        if matches!(timer.state, TimerState::Stopping(_)) {
            let configuration = u32::from(timer.vector.vector()) | MASKED;
            self.timer_write(LVT, configuration);
            self.timer_write(INITIAL, 0);
            if !self.timer_matches(LVT, LVT_FIELDS, configuration) || !self.timer_quiet() {
                return Err(Failure::Unconfirmed);
            }
        }
        self.timer
            .bound_mut()
            .expect("timer remains reserved")
            .state = TimerState::Stopped;
        Ok(())
    }

    /// Return the original vector only after stop and native IRQ drain.
    pub fn release_timer(&mut self) -> Result<VectorLease, Failure> {
        self.reception_context()?;
        let timer = self.timer.bound()?;
        let state = timer.state;
        let vector = timer.vector.vector();
        if !matches!(state, TimerState::Reserved | TimerState::Stopped)
            || !self.timer_quiet()
            || self.vector_busy(vector)
        {
            return Err(Failure::Busy);
        }
        let Slot::Bound(timer) = core::mem::replace(&mut self.timer, Slot::Vacant) else {
            unreachable!("timer remains reserved");
        };
        Ok(timer.vector)
    }

    fn timer_quiet(&mut self) -> bool {
        self.timer_matches(LVT, MASKED | DELIVERY_PENDING, MASKED)
            && self.timer_read(INITIAL) == 0
            && self.timer_read(CURRENT) == 0
    }

    fn timer_matches(&mut self, register: usize, mask: u32, expected: u32) -> bool {
        self.timer_read(register) & mask == expected
    }

    fn timer_read(&mut self, register: usize) -> u32 {
        unsafe { self.window.read_u32(register) }.expect("xAPIC timer register geometry")
    }

    fn timer_write(&mut self, register: usize, value: u32) {
        unsafe { self.window.write_u32(register, value) }.expect("xAPIC timer register geometry");
    }
}

#[cfg(test)]
#[path = "../../../../../../tests/metal/interrupt/x86_64/internal/apic/timer.rs"]
mod tests;
