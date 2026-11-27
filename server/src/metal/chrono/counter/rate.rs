//! Checked scaling of native counts at the admitted whole-Hertz rate.

use core::{num::NonZeroU64, time::Duration};

pub(in crate::metal::chrono) fn later_count(
    now: u64,
    frequency: NonZeroU64,
    delay: Duration,
) -> Option<u64> {
    let numerator = delay.as_nanos().checked_mul(u128::from(frequency.get()))?;
    let ticks = u64::try_from(numerator.div_ceil(1_000_000_000)).ok()?;
    now.checked_add(ticks).filter(|count| *count > now)
}

pub(in crate::metal::chrono) fn duration(ticks: u64, frequency: NonZeroU64) -> Duration {
    let hz = frequency.get();
    let nanos = u128::from(ticks % hz) * 1_000_000_000 / u128::from(hz);
    Duration::new(ticks / hz, nanos as u32)
}
