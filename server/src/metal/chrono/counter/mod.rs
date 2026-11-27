//! The original native read operation and its local execution eligibility.

use core::{marker::PhantomData, num::NonZeroU64};

use crate::metal::processor::treaty::ProcessorId;

pub(super) mod rate;

#[cfg_attr(
    not(any(limen_target, test)),
    expect(
        dead_code,
        reason = "only native or controlled observations admit a counter"
    )
)]
pub(super) struct Counter {
    processor: ProcessorId,
    frequency: NonZeroU64,
    read: fn() -> u64,
    same_processor: PhantomData<*mut ()>,
}

#[cfg_attr(
    not(any(limen_target, test)),
    expect(
        dead_code,
        reason = "only native or controlled observations admit and read a counter"
    )
)]
impl Counter {
    pub(super) const fn new(
        processor: ProcessorId,
        frequency: NonZeroU64,
        read: fn() -> u64,
    ) -> Self {
        Self {
            processor,
            frequency,
            read,
            same_processor: PhantomData,
        }
    }

    pub(super) const fn processor(&self) -> ProcessorId {
        self.processor
    }

    pub(super) const fn frequency(&self) -> NonZeroU64 {
        self.frequency
    }

    pub(super) fn observe(&self, current: ProcessorId) -> Option<u64> {
        (self.processor == current).then(self.read)
    }
}
