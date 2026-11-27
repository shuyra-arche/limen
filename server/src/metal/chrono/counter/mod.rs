//! The original native read operation and its local execution eligibility.

use core::{marker::PhantomData, num::NonZeroU64};

use crate::metal::processor::treaty::HartId;

pub(super) mod rate;

#[cfg_attr(
    not(any(limen_target, test)),
    expect(
        dead_code,
        reason = "only native or controlled observations admit a counter"
    )
)]
pub(super) struct Counter {
    hart: HartId,
    frequency: NonZeroU64,
    read: fn() -> u64,
    same_hart: PhantomData<*mut ()>,
}

#[cfg_attr(
    not(any(limen_target, test)),
    expect(
        dead_code,
        reason = "only native or controlled observations admit and read a counter"
    )
)]
impl Counter {
    pub(super) const fn new(hart: HartId, frequency: NonZeroU64, read: fn() -> u64) -> Self {
        Self {
            hart,
            frequency,
            read,
            same_hart: PhantomData,
        }
    }

    pub(super) const fn hart(&self) -> HartId {
        self.hart
    }

    pub(super) const fn frequency(&self) -> NonZeroU64 {
        self.frequency
    }

    pub(super) fn observe(&self, current: HartId) -> Option<u64> {
        (self.hart == current).then(self.read)
    }
}
