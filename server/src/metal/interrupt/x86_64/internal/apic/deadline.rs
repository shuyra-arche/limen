//! Absolute TSC comparisons on the original local APIC.

use super::Failure;
use crate::metal::interrupt::treaty::TimerEffect;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum State {
    Idle,
    Selecting(u64),
    Programming {
        previous: Option<u64>,
        requested: u64,
    },
    Armed(u64),
}

impl State {
    fn arm(self, now: u64, requested: u64) -> Result<Self, Failure> {
        if self != Self::Idle || requested <= now || requested == u64::MAX {
            return Err(Failure::Busy);
        }
        Ok(Self::Selecting(requested))
    }

    fn advance(self, now: u64, requested: u64) -> Result<Self, Failure> {
        let Self::Armed(previous) = self else {
            return Err(Failure::Busy);
        };
        if requested <= now || requested > previous || now >= previous {
            return Err(Failure::Busy);
        }
        if requested == previous {
            return Ok(self);
        }
        Ok(Self::Programming {
            previous: Some(previous),
            requested,
        })
    }

    fn selected(self) -> Result<Self, Failure> {
        let Self::Selecting(requested) = self else {
            return Err(Failure::Busy);
        };
        Ok(Self::Programming {
            previous: None,
            requested,
        })
    }

    fn effect(self) -> Option<TimerEffect> {
        match self {
            Self::Selecting(requested) => Some(TimerEffect::Programming {
                previous: None,
                requested,
            }),
            Self::Programming {
                previous,
                requested,
            } => Some(TimerEffect::Programming {
                previous,
                requested,
            }),
            Self::Idle | Self::Armed(_) => None,
        }
    }

    fn confirmed(self) -> Result<Option<u64>, Failure> {
        match self {
            Self::Idle => Ok(None),
            Self::Armed(target) => Ok(Some(target)),
            _ => Err(Failure::Busy),
        }
    }

    // The deadline MSR automatically becomes zero on expiration. A zero
    // alone cannot confirm programming, expiration, or native reception.
    fn observe(self, now: u64, msr: u64, pending: bool) -> Result<Self, Failure> {
        let Self::Programming { requested, .. } = self else {
            return Err(Failure::Busy);
        };
        if msr == requested || (msr == 0 && now >= requested && pending) {
            Ok(Self::Armed(requested))
        } else {
            Err(Failure::Unconfirmed)
        }
    }

    fn received(self, now: u64, msr: u64) -> bool {
        let target = match self {
            Self::Programming { requested, .. } | Self::Armed(requested) => requested,
            Self::Idle | Self::Selecting(_) => return false,
        };
        msr == 0 && now >= target
    }
}

#[cfg(all(limen_target, target_arch = "x86_64"))]
pub use native::DeadlineTimer;
#[cfg(all(limen_target, target_arch = "x86_64"))]
pub(super) use native::Record;

#[cfg(all(limen_target, target_arch = "x86_64"))]
mod native {
    use core::arch::{asm, x86_64::__cpuid};

    use super::{Failure, State, TimerEffect};
    use crate::metal::{
        chrono::{treaty::{Clock, Deadline}, x86_64::tsc::Tsc},
        interrupt::treaty::{AbsoluteTimer, absolute::Sealed},
        processor::treaty::Irq,
    };

    use super::super::{
        LocalApic, Reception, VectorLease,
        timer::{LVT, LVT_FIELDS, Slot},
    };

    const DEADLINE_MODE: u32 = 2 << 17;
    const DEADLINE_MSR: u32 = 0x6e0;

    pub(in crate::metal::interrupt::x86_64::internal::apic) struct Record {
        vector: VectorLease,
        state: State,
    }

    fn supported() -> bool {
        __cpuid(1).ecx & (1 << 24) != 0
    }

    fn read() -> u64 {
        let (low, high): (u32, u32);
        unsafe {
            asm!(
                "rdmsr",
                in("ecx") DEADLINE_MSR,
                out("eax") low,
                out("edx") high,
                options(nostack, preserves_flags),
            );
        }
        (u64::from(high) << 32) | u64::from(low)
    }

    fn write(count: u64) {
        // xAPIC LVT MMIO must precede the non-serializing deadline WRMSR.
        unsafe {
            asm!(
                "mfence",
                "wrmsr",
                in("ecx") DEADLINE_MSR,
                in("eax") count as u32,
                in("edx") (count >> 32) as u32,
                options(nostack, preserves_flags),
            );
        }
    }

    impl LocalApic<'_> {
        /// Observe the actual local capability without touching the MSR or
        /// transferring the original timer/vector ownership.
        pub fn deadline_supported(&self) -> Result<bool, Failure> {
            self.reception_context()?;
            Ok(supported())
        }
    }

    /// A resident xAPIC owner using the already admitted local TSC. The
    /// comparison and its vector stay in the original APIC timer slot.
    /// This ordinary-wake profile does not cancel active comparisons or
    /// return the vector to another source; late interrupts remain receivable.
    #[must_use]
    pub struct DeadlineTimer<'map, 'clock> {
        owner: LocalApic<'map>,
        clock: &'clock Clock,
    }

    impl<'map, 'clock> DeadlineTimer<'map, 'clock> {
        /// Transfer the existing APIC and its already leased vector.
        ///
        /// # Safety
        /// The original legacy timer has retired, and the caller maintains
        /// exclusive local APIC and MSR control, an admitted continuous TSC,
        /// a ready non-nested receiver for the exact vector, and the resident
        /// owner until every native and software obligation has settled.
        /// This profile retains the vector for its lifetime, not a hardware
        /// retirement certificate or a budget-stop timing guarantee.
        pub unsafe fn admit(
            mut owner: LocalApic<'map>,
            vector: VectorLease,
            tsc: &'clock Tsc,
        ) -> Result<Self, (LocalApic<'map>, VectorLease, Failure)> {
            let clock = tsc.clock();
            let reason = if owner.deadline_supported() != Ok(true)
                || clock.processor() != owner.processor
                || clock.sample().is_none()
                || !vector.matches(owner.processor as u8, vector.vector())
            {
                Some(Failure::Scope)
            } else if !matches!(owner.timer, Slot::Vacant)
                || owner.reception() != Reception::Idle
                || !owner.timer_quiet()
                || owner.vector_pending(vector.vector())
                || read() != 0
            {
                Some(Failure::Busy)
            } else {
                None
            };
            if let Some(reason) = reason {
                return Err((owner, vector, reason));
            }
            owner.timer = Slot::Deadline(Record {
                vector,
                state: State::Idle,
            });
            Ok(Self { owner, clock })
        }

        fn record(&self) -> &Record {
            match &self.owner.timer {
                Slot::Deadline(record) => record,
                _ => unreachable!("original deadline slot"),
            }
        }

        fn record_mut(&mut self) -> &mut Record {
            match &mut self.owner.timer {
                Slot::Deadline(record) => record,
                _ => unreachable!("original deadline slot"),
            }
        }

        pub fn vector(&self) -> u8 {
            self.record().vector.vector()
        }

        fn context(&self) -> Result<(), Failure> {
            if self.owner.deadline_supported() != Ok(true)
                || self.clock.processor() != self.owner.processor
            {
                return Err(Failure::Scope);
            }
            Ok(())
        }

        fn now(&self) -> Result<u64, Failure> {
            self.clock.sample().map(|sample| sample.count()).ok_or(Failure::Scope)
        }

        fn mode(&mut self) -> Result<(), Failure> {
            let configuration = DEADLINE_MODE | u32::from(self.vector());
            self.owner.timer_matches(LVT, LVT_FIELDS, configuration)
                .then_some(())
                .ok_or(Failure::Unconfirmed)
        }

        fn observe(&mut self) -> Result<(), Failure> {
            self.mode()?;
            let msr = read();
            let now = self.now()?;
            let pending = self.owner.vector_pending(self.vector());
            let state = self.record().state.observe(now, msr, pending)?;
            self.record_mut().state = state;
            Ok(())
        }

        fn select(&mut self) -> Result<(), Failure> {
            self.mode()?;
            let state = self.record().state.selected()?;
            let State::Programming { requested, .. } = state else {
                unreachable!("selected comparison");
            };
            self.record_mut().state = state;
            write(requested);
            self.observe()
        }

        fn reception_idle(&self) -> Result<(), Failure> {
            (self.owner.reception() == Reception::Idle)
                .then_some(())
                .ok_or(Failure::Busy)
        }

        /// Accept the actual vector, preserving the current comparison on
        /// stale hints. The original APIC reception owns EOI independently.
        /// # Safety
        /// The caller durably retains the ordinary deadline recheck for
        /// every accepted IRQ, even if EOI confirmation must be retried.
        pub unsafe fn accept_interrupt(
            &mut self,
            irq: &mut Irq<'_>,
        ) -> Result<bool, Failure> {
            self.owner.interrupt_context(irq)?;
            if irq.vector() != self.vector() || !supported() {
                return Err(Failure::Scope);
            }
            let state = self.record().state;
            let received = if matches!(state, State::Programming { .. } | State::Armed(_)) {
                self.mode()?;
                let now = self.clock.read_on_interrupt(irq).ok_or(Failure::Scope)?;
                state.received(now, read())
            } else {
                false
            };
            let completion = {
                let mut claim = unsafe { self.owner.receive_irq(irq) }?;
                unsafe { claim.complete() }
            };
            // Actual reception was recorded, regardless of the EOI result.
            // A later or duplicated IRQ cannot release this vector lease.
            if received {
                self.record_mut().state = State::Idle;
            }
            completion.map(|()| received)
        }

        /// Progress the original receiver's outstanding EOI only.
        /// # Safety
        /// The caller still retains the original ordinary recheck.
        pub unsafe fn settle_reception(&mut self) -> Result<(), Failure> {
            self.context()?;
            match self.owner.reception() {
                Reception::Idle => Ok(()),
                Reception::Received(_) => {
                    let mut claim = self.owner.current().ok_or(Failure::Busy)?;
                    unsafe { claim.complete() }
                }
                Reception::Unconfirmed(_) => {
                    self.owner.current().ok_or(Failure::Busy)?.confirm()
                }
                Reception::Exhausted(_) => Err(Failure::Busy),
            }
        }

        fn idle(&mut self) -> Result<(), Failure> {
            self.context()?;
            self.reception_idle()?;
            if self.record().state != State::Idle
                || self.owner.vector_pending(self.vector())
                || read() != 0
            {
                return Err(Failure::Busy);
            }
            Ok(())
        }
    }

    impl Sealed for DeadlineTimer<'_, '_> {}

    impl AbsoluteTimer for DeadlineTimer<'_, '_> {
        type Failure = Failure;

        fn arm_at(&mut self, deadline: Deadline<'_>) -> Result<(), Failure> {
            self.idle()?;
            let target = self.clock.count_for(deadline).ok_or(Failure::Scope)?;
            let state = self.record().state.arm(self.now()?, target)?;
            let configuration = DEADLINE_MODE | u32::from(self.vector());
            self.record_mut().state = state;
            self.owner.timer_write(LVT, configuration);
            self.select()
        }

        fn advance_deadline(&mut self, deadline: Deadline<'_>) -> Result<(), Failure> {
            self.context()?;
            self.reception_idle()?;
            self.mode()?;
            let target = self.clock.count_for(deadline).ok_or(Failure::Scope)?;
            let state = self.record().state;
            let previous = state.confirmed()?.ok_or(Failure::Busy)?;
            if self.owner.vector_pending(self.vector()) || read() != previous {
                return Err(Failure::Busy);
            }
            let next = state.advance(self.now()?, target)?;
            if next != state {
                self.record_mut().state = next;
                write(target);
                self.observe()?;
            }
            Ok(())
        }

        fn confirmed_deadline(&self) -> Result<Option<Deadline<'_>>, Failure> {
            self.context()?;
            self.reception_idle()?;
            Ok(self.record().state.confirmed()?.map(|count| self.clock.deadline(count)))
        }

        fn due(&self) -> Result<bool, Failure> {
            self.context()?;
            let target = self.record().state.confirmed()?.ok_or(Failure::Busy)?;
            Ok(self.now()? >= target)
        }

        fn pending_effect(&self) -> Result<Option<TimerEffect>, Failure> {
            self.context()?;
            Ok(self.record().state.effect())
        }

        fn confirm_effect(&mut self) -> Result<(), Failure> {
            self.context()?;
            self.reception_idle()?;
            match self.record().state {
                State::Selecting(_) => self.select(),
                State::Programming { .. } => self.observe(),
                _ => Err(Failure::Busy),
            }
        }

        unsafe fn clear(&mut self) -> Result<(), Failure> {
            // No active disarm or mode transition in this resident profile:
            // wait for the original native expiration and receive its IRQ.
            self.idle()
        }

        unsafe fn stop_due(&mut self) -> Result<(), Failure> {
            self.idle()
        }
    }
}

#[cfg(test)]
#[path = "../../../../../../tests/metal/interrupt/x86_64/internal/apic/deadline.rs"]
mod tests;
