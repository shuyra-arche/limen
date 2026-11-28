//! The original native read operation and its local execution eligibility.

use core::{marker::PhantomData, num::NonZeroU64};

#[cfg(all(limen_target, target_arch = "x86_64"))]
use crate::metal::memory::access::treaty::device::DeviceWindow;
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
    read: Read,
    same_processor: PhantomData<*mut ()>,
}

#[cfg_attr(
    not(any(limen_target, test)),
    expect(dead_code, reason = "only native or controlled counter observations")
)]
enum Read {
    Register(fn() -> u64),
    #[cfg(all(limen_target, target_arch = "x86_64"))]
    Mapped {
        window: DeviceWindow<'static>,
        observe: fn(&DeviceWindow<'_>) -> Option<u64>,
    },
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
            read: Read::Register(read),
            same_processor: PhantomData,
        }
    }

    #[cfg(all(limen_target, target_arch = "x86_64"))]
    pub(super) fn mapped(
        processor: ProcessorId,
        frequency: NonZeroU64,
        window: DeviceWindow<'static>,
        observe: fn(&DeviceWindow<'_>) -> Option<u64>,
    ) -> Self {
        Self {
            processor,
            frequency,
            read: Read::Mapped { window, observe },
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
        if self.processor != current {
            return None;
        }
        match &self.read {
            Read::Register(read) => Some(read()),
            #[cfg(all(limen_target, target_arch = "x86_64"))]
            Read::Mapped { window, observe } => observe(window),
        }
    }
}
