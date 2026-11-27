//! An admitted hardware counter and samples from its one local time domain.

use core::{marker::PhantomData, num::NonZeroU32, time::Duration};

use crate::metal::platform::description::devicetree::NodeId;

/// Sampling is local to the Hart qualified by the actual counter provider.
#[must_use]
pub struct Clock<'tree> {
    source: NodeId<'tree>,
    hart: usize,
    hertz: NonZeroU32,
    #[cfg(limen_target)]
    read: fn() -> u64,
    local: PhantomData<*mut ()>,
}

impl<'tree> Clock<'tree> {
    #[cfg(all(limen_target, any(target_arch = "riscv64", target_arch = "aarch64")))]
    pub(super) fn bind(
        source: NodeId<'tree>,
        hart: usize,
        hertz: NonZeroU32,
        read: fn() -> u64,
    ) -> Option<Self> {
        Self::current(hart).then_some(Self {
            source,
            hart,
            hertz,
            read,
            local: PhantomData,
        })
    }

    pub fn source(&self) -> NodeId<'tree> {
        self.source
    }

    pub fn hart(&self) -> usize {
        self.hart
    }

    pub fn frequency(&self) -> NonZeroU32 {
        self.hertz
    }

    pub(crate) fn deadline(&self, count: u64) -> Deadline<'_, 'tree> {
        Deadline { clock: self, count }
    }

    #[cfg(limen_target)]
    fn current(hart: usize) -> bool {
        crate::metal::processor::hart::local::try_current_hart()
            .is_some_and(|current| current.get() == hart)
    }

    #[cfg(limen_target)]
    pub(crate) fn read(&self) -> Option<u64> {
        Self::current(self.hart).then(self.read)
    }

    #[cfg(limen_target)]
    pub fn sample(&self) -> Option<Sample<'_, 'tree>> {
        Some(Sample {
            clock: self,
            count: self.read()?,
        })
    }
}

/// One observation. Comparison requires the same live admitted Clock.
#[derive(Clone, Copy)]
pub struct Sample<'clock, 'tree> {
    clock: &'clock Clock<'tree>,
    count: u64,
}

impl<'clock, 'tree> Sample<'clock, 'tree> {
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
    pub fn after(self, delay: Duration) -> Option<Deadline<'clock, 'tree>> {
        let count = later_count(self.count, self.clock.frequency(), delay)?;
        Some(self.clock.deadline(count))
    }
}

/// An exact comparison target issued by one admitted hardware counter.
#[derive(Clone, Copy)]
pub struct Deadline<'clock, 'tree> {
    clock: &'clock Clock<'tree>,
    count: u64,
}

impl Deadline<'_, '_> {
    /// Return the native comparison count in its original clock domain.
    pub fn count_for(self, clock: &Clock<'_>) -> Option<u64> {
        core::ptr::eq(self.clock, clock).then_some(self.count)
    }
}

fn later_count(now: u64, frequency: NonZeroU32, delay: Duration) -> Option<u64> {
    let numerator = delay.as_nanos().checked_mul(u128::from(frequency.get()))?;
    let ticks = u64::try_from(numerator.div_ceil(1_000_000_000)).ok()?;
    now.checked_add(ticks).filter(|count| *count > now)
}

fn duration(ticks: u64, frequency: NonZeroU32) -> Duration {
    let hz = u64::from(frequency.get());
    let nanos = ticks % hz * 1_000_000_000 / hz;
    Duration::new(ticks / hz, nanos as u32)
}

#[cfg(test)]
#[path = "../../../tests/metal/chrono/clock.rs"]
mod tests;
