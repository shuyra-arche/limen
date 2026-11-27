//! An admitted hardware counter and samples from its one local time domain.

use core::{marker::PhantomData, num::NonZeroU64, time::Duration};

#[cfg(limen_target)]
use crate::metal::processor::hart::local::{HartId, try_current_hart};

/// One admitted counter and time domain, bound to its verified local Hart.
#[must_use]
pub struct Clock {
    #[cfg(limen_target)]
    hart: HartId,
    hertz: NonZeroU64,
    #[cfg(limen_target)]
    read: fn() -> u64,
    local: PhantomData<*mut ()>,
}

impl Clock {
    #[cfg(limen_target)]
    pub(super) fn bind(hart: HartId, hertz: NonZeroU64, read: fn() -> u64) -> Option<Self> {
        (try_current_hart() == Some(hart)).then_some(Self {
            hart,
            hertz,
            read,
            local: PhantomData,
        })
    }

    #[cfg(limen_target)]
    pub fn hart(&self) -> usize {
        self.hart.get()
    }

    pub fn frequency(&self) -> NonZeroU64 {
        self.hertz
    }

    pub(crate) fn deadline(&self, count: u64) -> Deadline<'_> {
        Deadline { clock: self, count }
    }

    #[cfg(limen_target)]
    pub(crate) fn read(&self) -> Option<u64> {
        (try_current_hart() == Some(self.hart)).then(self.read)
    }

    #[cfg(limen_target)]
    pub fn sample(&self) -> Option<Sample<'_>> {
        Some(Sample {
            clock: self,
            count: self.read()?,
        })
    }
}

/// One observation. Comparison requires the same live admitted Clock.
#[derive(Clone, Copy)]
pub struct Sample<'clock> {
    clock: &'clock Clock,
    count: u64,
}

impl<'clock> Sample<'clock> {
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

/// An exact comparison target issued by one admitted hardware counter.
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
