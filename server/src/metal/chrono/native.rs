//! Read access to one original native counter on the verified local Hart.

use core::num::NonZeroU64;

use super::{Clock, Deadline, Sample};
use crate::metal::processor::hart::{HartId, local::try_current_hart};

#[must_use]
pub struct LocalClock {
    clock: Clock,
    read: fn() -> u64,
}

impl LocalClock {
    pub(super) fn bind(hart: HartId, hertz: NonZeroU64, read: fn() -> u64) -> Option<Self> {
        (try_current_hart() == Some(hart)).then_some(Self {
            clock: Clock::new(hart, hertz),
            read,
        })
    }

    pub fn hart(&self) -> usize {
        self.clock.hart().get()
    }

    pub fn frequency(&self) -> NonZeroU64 {
        self.clock.frequency()
    }

    pub fn sample(&self) -> Option<Sample<'_>> {
        self.clock.observe(try_current_hart()?, self.read)
    }

    pub fn read(&self) -> Option<u64> {
        self.sample().map(Sample::count)
    }

    #[cfg(target_arch = "riscv64")]
    pub(super) fn deadline(&self, count: u64) -> Deadline<'_> {
        self.clock.deadline(count)
    }

    /// Recover the comparison count for this exact native counter.
    pub fn count_for(&self, deadline: Deadline<'_>) -> Option<u64> {
        deadline.count_for(&self.clock)
    }
}
