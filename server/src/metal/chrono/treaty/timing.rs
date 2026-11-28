//! Conservative time bounds for one continuous local counter.

use core::{num::NonZeroU64, ops::RangeInclusive, time::Duration};

use super::super::counter::rate::duration;
#[cfg(limen_target)]
use super::{Clock, Deadline, Sample};

const NANOS_PER_SECOND: u128 = 1_000_000_000;

/// Bounds supplied by the trusted owner of the local timing profile.
pub struct TimingBounds {
    pub minimum_hertz: NonZeroU64,
    pub maximum_hertz: NonZeroU64,
    /// Maximum error of each admitted read in native ticks, excluding
    /// quantization. One additional tick covers the sample's phase.
    pub count_error: u64,
    /// Inclusive interval of the original counter with proven continuity.
    pub valid: RangeInclusive<u64>,
}

/// Worst-case bounds for one admitted execution and stop path.
#[derive(Clone, Copy)]
pub struct StopBounds {
    /// Elapsed time from the comparison condition to closed measurement.
    pub elapsed: Duration,
    /// Maximum conservative charge from the comparison condition to closed
    /// measurement, including the profile's accounting and timing error.
    pub charged: Duration,
    /// Elapsed time from the final Start sample to entry into the covered
    /// execution region with the original interrupt path available.
    pub start: Duration,
}

/// An outward-rounded interval for measured occupancy time.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DurationBounds {
    pub minimum: Duration,
    pub maximum: Duration,
}

impl TimingBounds {
    fn valid(&self, nominal: NonZeroU64) -> bool {
        self.minimum_hertz <= nominal
            && nominal <= self.maximum_hertz
            && self.valid.start() < self.valid.end()
            && self
                .error()
                .and_then(|error| error.checked_mul(2))
                .is_some()
    }

    fn error(&self) -> Option<u64> {
        self.count_error.checked_add(1)
    }

    fn window_valid(&self, opens: u64, shuts: u64) -> bool {
        opens < shuts && self.valid.contains(&opens) && self.valid.contains(&shuts)
    }

    fn observed(&self, count: u64) -> Option<RangeInclusive<u64>> {
        let error = self.error()?;
        let lower = count.checked_sub(error)?;
        let upper = count.checked_add(error)?;
        self.window_valid(lower, upper).then_some(lower..=upper)
    }

    fn in_window(&self, count: u64, opens: u64, shuts: u64) -> bool {
        self.window_valid(opens, shuts)
            && self
                .observed(count)
                .is_some_and(|count| *count.start() >= opens && *count.end() < shuts)
    }

    fn trigger(&self, start: u64, shuts: u64, ceiling: Duration, stop: StopBounds) -> Option<u64> {
        let start = self.observed(start)?;
        if !self.valid.contains(&shuts)
            || stop.elapsed.is_zero()
            || stop.charged.is_zero()
            || stop.start.is_zero()
        {
            return None;
        }
        let time = ceiling.checked_sub(stop.charged)?;
        // With the slowest admissible clock, the comparison still arrives
        // within this budget. Leave headroom for the bound of a read at the
        // trigger as well as a strict margin of one tick.
        let spread = self.error()?.checked_mul(2)?;
        let spend = ticks(time, self.minimum_hertz, false)?
            .checked_sub(spread)?
            .checked_sub(1)?;
        let budget = start.start().checked_add(spend)?;
        // With the fastest admissible clock, the full stop tail still
        // completes before the original window shuts. Keep the ending
        // sample, including its uncertainty, inside the valid interval.
        let tail = ticks(stop.elapsed, self.maximum_hertz, true)?
            .checked_add(spread)?
            .checked_add(1)?;
        let window = shuts.checked_sub(tail)?;
        let target = budget.min(window);
        self.can_start(*start.end(), target, stop.start)
            .then_some(target)
    }

    fn can_start(&self, upper: u64, target: u64, start: Duration) -> bool {
        let Some(lead) = ticks(start, self.maximum_hertz, true) else {
            return false;
        };
        upper
            .checked_add(lead)
            .is_some_and(|latest_entry| latest_entry < target)
    }

    fn measure(&self, start: u64, end: u64) -> Option<DurationBounds> {
        // Reject a backwards native count even if the error intervals would
        // overlap. Trustworthy continuity across a wrap needs another profile.
        let difference = end.checked_sub(start)?;
        let start = self.observed(start)?;
        let end = self.observed(end)?;
        let maximum = end.end().checked_sub(*start.start())?;
        let minimum = end.start().saturating_sub(*start.end());
        debug_assert!(minimum <= difference && difference <= maximum);
        Some(DurationBounds {
            minimum: duration(minimum, self.maximum_hertz),
            maximum: duration_ceil(maximum, self.minimum_hertz)?,
        })
    }
}

/// A local timing profile for the existing original Clock.
#[cfg(limen_target)]
#[must_use]
pub struct LocalTiming<'clock> {
    clock: &'clock Clock,
    bounds: TimingBounds,
}

#[cfg(limen_target)]
impl<'clock> LocalTiming<'clock> {
    /// Admit a platform-supplied timing profile on this Clock's processor.
    ///
    /// # Safety
    /// The platform must own the actual source and maintain the stated
    /// frequency, read error, uninterrupted count progression and validity
    /// under all admitted power, firmware and execution conditions. The
    /// nominal frequency in the native description alone does not prove
    /// these bounds. The original validity interval must not repeat after
    /// reset or wrap while this binding is in use.
    pub unsafe fn admit(clock: &'clock Clock, bounds: TimingBounds) -> Option<Self> {
        if !bounds.valid(clock.frequency()) {
            return None;
        }
        bounds.observed(clock.sample()?.count())?;
        Some(Self { clock, bounds })
    }

    pub fn clock(&self) -> &'clock Clock {
        self.clock
    }

    /// Observe the original local counter while the timing profile is valid.
    pub fn sample(&self) -> Option<Sample<'clock>> {
        let sample = self.clock.sample()?;
        self.bounds.observed(sample.count())?;
        Some(sample)
    }

    pub fn window_valid(&self, opens: Deadline<'_>, shuts: Deadline<'_>) -> bool {
        let Some(opens) = self.clock.count_for(opens) else {
            return false;
        };
        let Some(shuts) = self.clock.count_for(shuts) else {
            return false;
        };
        self.bounds.window_valid(opens, shuts)
    }

    pub fn in_window(
        &self,
        observed: Sample<'_>,
        opens: Deadline<'_>,
        shuts: Deadline<'_>,
    ) -> bool {
        let Some(count) = observed.count_for(self.clock) else {
            return false;
        };
        let Some(opens) = self.clock.count_for(opens) else {
            return false;
        };
        let Some(shuts) = self.clock.count_for(shuts) else {
            return false;
        };
        self.bounds.in_window(count, opens, shuts)
    }

    /// Choose an early comparison for the original elapsed and charge limits.
    /// The execution owner supplies independently admitted stop bounds and
    /// still owns programming and confirmation of its actual comparator.
    pub fn stop(
        &self,
        observed: Sample<'_>,
        shuts: Deadline<'_>,
        ceiling: Duration,
        stop: StopBounds,
    ) -> Option<Deadline<'clock>> {
        let observed = observed.count_for(self.clock)?;
        let shuts = self.clock.count_for(shuts)?;
        let target = self.bounds.trigger(observed, shuts, ceiling, stop)?;
        (target < u64::MAX).then(|| self.clock.deadline(target))
    }

    /// Recheck the exact confirmed original comparison against a fresh
    /// sample and the certified remaining Start tail.
    pub fn can_start(
        &self,
        observed: Sample<'_>,
        opens: Deadline<'_>,
        planned: Deadline<'_>,
        confirmed: Deadline<'_>,
        start: Duration,
    ) -> bool {
        let Some(upper) = observed
            .count_for(self.clock)
            .and_then(|count| self.bounds.observed(count))
            .map(|count| *count.end())
        else {
            return false;
        };
        let Some(target) = self.clock.count_for(planned) else {
            return false;
        };
        self.clock.count_for(confirmed) == Some(target)
            && self.in_window(observed, opens, planned)
            && self.bounds.can_start(upper, target, start)
    }

    /// Bound the elapsed occupancy interval using its two actual readings.
    pub fn measure(&self, start: Sample<'_>, end: Sample<'_>) -> Option<DurationBounds> {
        self.bounds
            .measure(start.count_for(self.clock)?, end.count_for(self.clock)?)
    }
}

fn ticks(time: Duration, hertz: NonZeroU64, round_up: bool) -> Option<u64> {
    let product = time.as_nanos().checked_mul(u128::from(hertz.get()))?;
    let count = if round_up {
        product.div_ceil(NANOS_PER_SECOND)
    } else {
        product / NANOS_PER_SECOND
    };
    count.try_into().ok()
}

fn duration_ceil(ticks: u64, hertz: NonZeroU64) -> Option<Duration> {
    let nanos = (u128::from(ticks) * NANOS_PER_SECOND).div_ceil(u128::from(hertz.get()));
    let seconds = (nanos / NANOS_PER_SECOND).try_into().ok()?;
    Some(Duration::new(seconds, (nanos % NANOS_PER_SECOND) as u32))
}

#[cfg(test)]
#[path = "../../../../tests/metal/chrono/timing.rs"]
mod tests;
