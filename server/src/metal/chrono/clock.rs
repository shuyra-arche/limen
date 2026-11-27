//! Local counter domains and their observation and comparison rules.

use core::{marker::PhantomData, num::NonZeroU64, time::Duration};

use crate::metal::processor::hart::HartId;

/// One local counter domain with an exact whole-Hertz frequency.
/// A native counter keeps its own domain private.
#[must_use]
pub struct Clock {
    hart: HartId,
    hertz: NonZeroU64,
    local: PhantomData<*mut ()>,
}

impl Clock {
    pub const fn new(hart: HartId, hertz: NonZeroU64) -> Self {
        Self {
            hart,
            hertz,
            local: PhantomData,
        }
    }

    pub const fn hart(&self) -> HartId {
        self.hart
    }

    pub fn frequency(&self) -> NonZeroU64 {
        self.hertz
    }

    pub(super) fn deadline(&self, count: u64) -> Deadline<'_> {
        Deadline { clock: self, count }
    }

    /// Interpret a count reported for this Hart. Read the source only after
    /// checking the supplied Hart identity.
    pub fn observe(&self, hart: HartId, read: impl FnOnce() -> u64) -> Option<Sample<'_>> {
        (hart == self.hart).then(|| Sample {
            clock: self,
            count: read(),
        })
    }
}

/// One observation. Comparison requires the same live clock domain.
#[derive(Clone, Copy)]
pub struct Sample<'clock> {
    clock: &'clock Clock,
    count: u64,
}

impl<'clock> Sample<'clock> {
    pub fn count(self) -> u64 {
        self.count
    }

    /// Elapsed time rounded down to a nanosecond. Stale ordering, another
    /// clock or a wrapped counter fails instead of implying calibration.
    pub fn elapsed_since(self, earlier: Self) -> Option<Duration> {
        if !core::ptr::eq(self.clock, earlier.clock) {
            return None;
        }
        Some(duration(
            self.count.checked_sub(earlier.count)?,
            self.clock.hertz,
        ))
    }

    /// A later comparison in this same time domain. Rounds up so the
    /// number is never earlier than the requested elapsed duration.
    pub fn after(self, delay: Duration) -> Option<Deadline<'clock>> {
        let count = later_count(self.count, self.clock.frequency(), delay)?;
        Some(self.clock.deadline(count))
    }
}

/// An exact comparison target in its original clock domain.
#[derive(Clone, Copy)]
pub struct Deadline<'clock> {
    clock: &'clock Clock,
    count: u64,
}

impl Deadline<'_> {
    /// Return the native comparison count in its original clock domain.
    pub fn count_for(self, clock: &Clock) -> Option<u64> {
        core::ptr::eq(self.clock, clock).then_some(self.count)
    }
}

fn later_count(now: u64, frequency: NonZeroU64, delay: Duration) -> Option<u64> {
    let numerator = delay.as_nanos().checked_mul(u128::from(frequency.get()))?;
    let ticks = u64::try_from(numerator.div_ceil(1_000_000_000)).ok()?;
    now.checked_add(ticks).filter(|count| *count > now)
}

fn duration(ticks: u64, frequency: NonZeroU64) -> Duration {
    let hz = frequency.get();
    let nanos = u128::from(ticks % hz) * 1_000_000_000 / u128::from(hz);
    Duration::new(ticks / hz, nanos as u32)
}

#[cfg(test)]
#[path = "../../../tests/metal/chrono/clock.rs"]
mod tests;
