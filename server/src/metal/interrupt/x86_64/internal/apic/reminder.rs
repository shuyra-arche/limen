//! Ordinary absolute-deadline reminders on the actual relative xAPIC timer.
//! The reference determines expiry; the countdown supplies only a wake hint.

use core::num::NonZeroU32;

#[derive(Clone, Copy)]
struct Observation {
    before: u64,
    current: u32,
    after: u64,
}

#[derive(Clone, Copy)]
struct Rate {
    // The observed lower ratio of divided APIC ticks to reference ticks.
    // This is not a guarantee about future hardware behavior.
    decrement: u32,
    reference: u64,
}

impl Rate {
    fn observe(first: Observation, last: Observation) -> Option<Self> {
        if first.before > first.after
            || first.after >= last.before
            || last.before > last.after
            || last.current == 0
        {
            return None;
        }
        // Include a native-count phase tick and a reference-count phase
        // tick. Reject a zero/underflowing observed lower rate.
        let decrement = first.current.checked_sub(last.current)?.checked_sub(1)?;
        let reference = last.after.checked_sub(first.before)?.checked_add(1)?;
        (decrement != 0).then_some(Self {
            decrement,
            reference,
        })
    }

    fn ticks(self, remaining: u64) -> Option<NonZeroU32> {
        if remaining == 0 {
            return None;
        }
        let scaled = u128::from(remaining) * u128::from(self.decrement);
        let ticks = scaled / u128::from(self.reference);
        // Long waits are segmented. A positive remainder always gets a
        // nonzero reminder, including a sub-tick remainder.
        NonZeroU32::new(ticks.clamp(1, u128::from(u32::MAX)) as u32)
    }
}

#[cfg(all(limen_target, target_arch = "x86_64"))]
pub use native::CountdownTimer;

#[cfg(all(limen_target, target_arch = "x86_64"))]
mod native {
    use super::{Observation, Rate};
    use super::super::{
        Countdown, Divider, Failure, LocalApic, Reception, TimerState,
        timer::CURRENT,
    };
    use crate::metal::{
        chrono::treaty::{Clock, Deadline},
        interrupt::treaty::{
            Reminder, ReminderError, ReminderProgress as Progress, reminder::Sealed,
        },
        processor::treaty::Irq,
    };
    use core::num::NonZeroU32;

    const DIVIDER: Divider = Divider::Sixteen;
    const OBSERVATIONS: usize = 1_000_000;

    struct Request {
        target: u64,
        received: bool,
    }

    /// The original controller, one leased timer vector, and an independently
    /// admitted reference. Only the physical countdown is authoritative for
    /// native expiration; the requested absolute value is a recheck hint.
    #[must_use]
    pub struct CountdownTimer<'map, 'clock> {
        owner: LocalApic<'map>,
        clock: &'clock Clock,
        rate: Rate,
        request: Option<Request>,
    }

    impl<'map, 'clock> CountdownTimer<'map, 'clock> {
        /// Measure the actual divided local timer against the exact reference.
        /// Return the original controller and its effects on every failure.
        ///
        /// # Safety
        /// The caller owns the original reserved timer/vector and an installed
        /// non-nested handler. Ordinary interrupts remain masked during
        /// calibration; if the timer unexpectedly expires, retain and settle
        /// its original IRQ with the returned owner. The platform maintains
        /// reference continuity, countdown progress and native delivery for
        /// the admitted profile. The observed ratio is only a reminder
        /// estimate, never a physical stop bound or a cross-clock certificate.
        pub unsafe fn calibrate(
            mut owner: LocalApic<'map>,
            clock: &'clock Clock,
        ) -> Result<Self, (LocalApic<'map>, Failure)> {
            let measured = (|| {
                owner.reception_context()?;
                if owner.processor != clock.processor()
                    || clock.sample().is_none()
                    || owner.reception() != Reception::Idle
                    || !matches!(
                        owner.timer_state(),
                        Some(TimerState::Reserved | TimerState::Stopped)
                    )
                {
                    return Err(Failure::Scope);
                }
                let vector = owner.timer.bound()?.vector.vector();
                unsafe {
                    owner.arm_timer(Countdown {
                        ticks: NonZeroU32::MAX,
                        divider: DIVIDER,
                    })
                }?;
                // Capture two actual decrement reads bracketed by reference
                // observations. A bounded attempt measures a useful interval
                // without depending on the relative timer for its own wait.
                let rate = (|| {
                    let first = observe(&mut owner, clock)?;
                    let span = (clock.frequency().get() / 50).max(1);
                    for _ in 0..OBSERVATIONS {
                        let last = observe(&mut owner, clock)?;
                        if last.before.checked_sub(first.after).ok_or(Failure::Scope)? >= span {
                            return Rate::observe(first, last).ok_or(Failure::Unconfirmed);
                        }
                        core::hint::spin_loop();
                    }
                    Err(Failure::Unconfirmed)
                })();
                // No original effect may be forgotten even if measuring fails.
                owner.stop_timer()?;
                if owner.vector_pending(vector) || owner.reception() != Reception::Idle {
                    return Err(Failure::Busy);
                }
                rate
            })();
            match measured {
                Ok(rate) => Ok(Self {
                    owner,
                    clock,
                    rate,
                    request: None,
                }),
                Err(reason) => Err((owner, reason)),
            }
        }

        pub fn clock(&self) -> &Clock {
            self.clock
        }

        pub fn vector(&self) -> u8 {
            self.owner.timer.bound().expect("original countdown").vector.vector()
        }

        /// Observe the last confirmed actual countdown, not an absolute
        /// deadline or a promise about its wall-clock delivery.
        pub fn countdown(&self) -> Option<Countdown> {
            match self.owner.timer_state() {
                Some(TimerState::Armed(countdown)) => Some(countdown),
                _ => None,
            }
        }

        fn context(&self) -> Result<(), Failure> {
            self.owner.reception_context()?;
            if self.clock.processor() != self.owner.processor {
                return Err(Failure::Scope);
            }
            Ok(())
        }

        fn now(&self) -> Result<u64, Failure> {
            self.clock
                .sample()
                .map(|sample| sample.count())
                .ok_or(Failure::Scope)
        }

        /// Receive the original vector. Retain an expired setting until the
        /// actual receipt and its EOI, independently of the Sign deadline.
        /// # Safety
        /// The original caller durably retains ordinary recheck responsibility
        /// and will not permit a nested borrow of this native owner.
        pub unsafe fn accept_interrupt(&mut self, irq: &mut Irq<'_>) -> Result<(), Failure> {
            self.owner.interrupt_context(irq)?;
            if irq.vector() != self.vector() {
                return Err(Failure::Scope);
            }
            let expired = self.owner.timer_read(CURRENT) == 0;
            let mut claim = unsafe { self.owner.receive_irq(irq) }?;
            if expired && let Some(request) = &mut self.request {
                request.received = true;
            }
            unsafe { claim.complete() }
        }

        fn settle_reception(&mut self) -> Result<(), Failure> {
            match self.owner.reception() {
                Reception::Idle => Ok(()),
                // Only accept_interrupt records this original obligation.
                Reception::Received(_) => {
                    let mut claim = self.owner.current().ok_or(Failure::Busy)?;
                    unsafe { claim.complete() }
                }
                Reception::Unconfirmed(_) => self.owner.current().ok_or(Failure::Busy)?.confirm(),
                Reception::Exhausted(_) => Err(Failure::Busy),
            }
        }

        fn progress(&mut self) -> Result<Progress, Failure> {
            self.context()?;
            self.settle_reception()?;
            if matches!(
                self.owner.timer_state(),
                Some(TimerState::Arming(_) | TimerState::Stopping(_))
            ) {
                self.owner.confirm_timer()?;
            }
            if self.owner.vector_pending(self.vector()) {
                return Ok(Progress::AwaitInterrupt);
            }
            match self.owner.timer_state() {
                Some(TimerState::Reserved | TimerState::Stopped) => {
                    self.request = None;
                    Ok(Progress::Idle)
                }
                Some(TimerState::Armed(_)) => {
                    let request = self.request.as_ref().ok_or(Failure::Scope)?;
                    if self.owner.timer_read(CURRENT) != 0 {
                        return Ok(Progress::Armed);
                    }
                    if !request.received {
                        return Ok(Progress::AwaitInterrupt);
                    }
                    self.stop()
                }
                _ => Err(Failure::Unconfirmed),
            }
        }

        fn stop(&mut self) -> Result<Progress, Failure> {
            self.owner.stop_timer()?;
            if self.owner.vector_pending(self.vector())
                || self.owner.reception() != Reception::Idle
            {
                // Keep the old identity through a raced expiration. Its
                // late vector must be received before the next arm.
                return Ok(Progress::AwaitInterrupt);
            }
            self.request = None;
            Ok(Progress::Idle)
        }

        fn recheck_local(&mut self, next: Option<Deadline<'_>>) -> Result<Progress, Failure> {
            self.context()?;
            let next = next
                .map(|deadline| self.clock.count_for(deadline).ok_or(Failure::Scope))
                .transpose()?;
            let progress = self.progress()?;
            if progress == Progress::AwaitInterrupt {
                return Ok(progress);
            }
            let Some(target) = next else {
                // A live old reminder may naturally expire. Do not cancel
                // it solely because the original business wait has settled.
                return Ok(progress);
            };
            if self.now()? >= target {
                return Ok(Progress::Recheck);
            }
            if progress == Progress::Armed {
                let request = self.request.as_ref().ok_or(Failure::Scope)?;
                if request.target <= target {
                    return Ok(progress);
                }
                // Earlier ordinary obligation: stop the old physical
                // countdown, not the old Sign registration.
                let stopped = self.stop()?;
                if stopped != Progress::Idle {
                    return Ok(stopped);
                }
            }
            let now = self.now()?;
            let Some(remaining) = target.checked_sub(now).filter(|remaining| *remaining != 0)
            else {
                return Ok(Progress::Recheck);
            };
            let ticks = self.rate.ticks(remaining).ok_or(Failure::Scope)?;
            self.request = Some(Request {
                target,
                received: false,
            });
            let result = unsafe {
                self.owner.arm_timer(Countdown {
                    ticks,
                    divider: DIVIDER,
                })
            };
            // A refusal before publication leaves no new hardware setting;
            // an unconfirmed original write keeps its exact request.
            if result.is_err()
                && matches!(
                    self.owner.timer_state(),
                    Some(TimerState::Reserved | TimerState::Stopped)
                )
            {
                self.request = None;
                if self.owner.vector_pending(self.vector()) {
                    return Ok(Progress::AwaitInterrupt);
                }
            }
            result?;
            Ok(Progress::Armed)
        }
    }

    impl Sealed for CountdownTimer<'_, '_> {}

    impl Reminder for CountdownTimer<'_, '_> {
        type Failure = Failure;

        fn recheck(
            &mut self,
            clock: &Clock,
            next: Option<Deadline<'_>>,
        ) -> Result<Progress, ReminderError<Failure>> {
            if !core::ptr::eq(self.clock, clock)
                || next.is_some_and(|deadline| self.clock.count_for(deadline).is_none())
            {
                return Err(ReminderError::Timing);
            }
            self.recheck_local(next).map_err(ReminderError::Timer)
        }
    }

    fn observe(owner: &mut LocalApic<'_>, clock: &Clock) -> Result<Observation, Failure> {
        let before = clock.sample().ok_or(Failure::Scope)?.count();
        let current = owner.timer_read(CURRENT);
        let after = clock.sample().ok_or(Failure::Scope)?.count();
        if before > after || current == 0 {
            return Err(Failure::Unconfirmed);
        }
        Ok(Observation {
            before,
            current,
            after,
        })
    }
}

#[cfg(test)]
#[path = "../../../../../../tests/metal/interrupt/x86_64/internal/apic/reminder.rs"]
mod tests;
