//! A clock observation from the verified local machine entry.

use core::time::Duration;

use crate::metal::processor::treaty::HartId;
#[cfg(limen_target)]
use crate::metal::processor::treaty::try_current_hart;

use super::super::counter::{
    Counter,
    rate::{duration, later_count},
};

/// One admitted local counter with a checked fixed conversion rate.
#[must_use]
pub struct Clock {
    counter: Counter,
}

impl Clock {
    /// The Hart on which this counter was admitted.
    pub fn hart(&self) -> usize {
        self.counter.hart().get()
    }

    pub fn frequency(&self) -> core::num::NonZeroU64 {
        self.counter.frequency()
    }
}

#[cfg(limen_target)]
impl Clock {
    pub(in crate::metal::chrono) fn bind(
        hart: HartId,
        frequency: core::num::NonZeroU64,
        read: fn() -> u64,
    ) -> Option<Self> {
        (try_current_hart() == Some(hart)).then_some(Self {
            counter: Counter::new(hart, frequency, read),
        })
    }

    /// Read a local count with the original counter and its admitted rate.
    pub fn sample(&self) -> Option<Sample<'_>> {
        Sample::reported(&self.counter, try_current_hart()?)
    }

    #[cfg(target_arch = "riscv64")]
    pub(in crate::metal::chrono) fn deadline(&self, count: u64) -> Deadline<'_> {
        Deadline::issued(&self.counter, count)
    }

    #[cfg(any(target_arch = "riscv64", target_arch = "aarch64"))]
    pub(in crate::metal) fn read(&self) -> Option<u64> {
        self.sample().map(Sample::count)
    }

    /// Recover the comparison count for this exact native counter.
    #[cfg(any(target_arch = "riscv64", target_arch = "aarch64"))]
    pub(in crate::metal) fn count_for(&self, deadline: Deadline<'_>) -> Option<u64> {
        deadline.count_for(&self.counter)
    }
}

/// A raw observation from the original admitted counter.
#[derive(Clone, Copy)]
pub struct Sample<'clock> {
    counter: &'clock Counter,
    count: u64,
}

impl<'clock> Sample<'clock> {
    #[cfg_attr(
        not(any(limen_target, test)),
        expect(
            dead_code,
            reason = "only a native or controlled source provides a count"
        )
    )]
    fn reported(counter: &'clock Counter, current: HartId) -> Option<Self> {
        Some(Self {
            counter,
            count: counter.observe(current)?,
        })
    }

    pub(super) fn count(self) -> u64 {
        self.count
    }

    /// Elapsed time rounded down to a nanosecond. Stale ordering, another
    /// counter or a wrapped counter fails instead of implying calibration.
    pub fn elapsed_since(self, earlier: Self) -> Option<Duration> {
        if !core::ptr::eq(self.counter, earlier.counter) {
            return None;
        }
        Some(duration(
            self.count().checked_sub(earlier.count())?,
            self.counter.frequency(),
        ))
    }

    /// A later comparison for the same original counter. Rounds up so the
    /// number is never earlier than the requested elapsed duration.
    pub fn after(self, delay: Duration) -> Option<Deadline<'clock>> {
        let count = later_count(self.count(), self.counter.frequency(), delay)?;
        Some(Deadline::issued(self.counter, count))
    }
}

/// An exact comparison target for its original counter.
#[cfg_attr(
    not(any(
        test,
        all(limen_target, any(target_arch = "aarch64", target_arch = "riscv64"))
    )),
    expect(
        dead_code,
        reason = "only an available native comparator or its controlled observation inspects the target"
    )
)]
#[derive(Clone, Copy)]
pub struct Deadline<'clock> {
    counter: &'clock Counter,
    count: u64,
}

impl<'clock> Deadline<'clock> {
    pub(super) const fn issued(counter: &'clock Counter, count: u64) -> Self {
        Self { counter, count }
    }

    /// Recover a comparison only within its original counter.
    #[cfg_attr(
        not(any(
            test,
            all(limen_target, any(target_arch = "aarch64", target_arch = "riscv64"))
        )),
        expect(
            dead_code,
            reason = "only an available native comparator or its controlled observation inspects the target"
        )
    )]
    pub(super) fn count_for(self, counter: &Counter) -> Option<u64> {
        core::ptr::eq(self.counter, counter).then_some(self.count)
    }
}

#[cfg(test)]
#[path = "../../../../tests/metal/chrono/clock.rs"]
mod tests;
