//! The original masked route and its destination's retirement observation.

use limen_treaty::model::PhysicalRange;

/// Physical scope observed through a locally admitted Group 1 interface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Destination {
    pub(in crate::metal::interrupt::aarch64::gic) domain: PhysicalRange,
    pub(in crate::metal::interrupt::aarch64::gic) frame: PhysicalRange,
    pub(in crate::metal::interrupt::aarch64::gic) affinity: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RetirementKey {
    pub(super) distributor: PhysicalRange,
    pub(super) slot: usize,
    pub(super) generation: u64,
    pub(super) intid: usize,
    pub(super) destination: Destination,
}

/// The permanent mask of one original SPI binding awaits its actual receiver.
#[must_use]
#[derive(Debug)]
#[cfg_attr(
    not(all(limen_target, target_arch = "aarch64")),
    expect(
        dead_code,
        reason = "the original CPU interface consumes requests on bare AArch64"
    )
)]
pub struct RetirementRequest(pub(super) RetirementKey);

impl RetirementRequest {
    #[cfg(all(limen_target, target_arch = "aarch64"))]
    pub(in crate::metal::interrupt::aarch64::gic) fn destination(&self) -> Destination {
        self.0.destination
    }

    #[cfg(all(limen_target, target_arch = "aarch64"))]
    pub(in crate::metal::interrupt::aarch64::gic) fn observed(
        self,
        received_through: u64,
    ) -> RetirementReceipt {
        RetirementReceipt {
            key: self.0,
            received_through,
        }
    }
}

/// The actual destination had no unresolved reception after the route closed.
#[must_use]
#[derive(Debug)]
pub struct RetirementReceipt {
    pub(super) key: RetirementKey,
    received_through: u64,
}

impl RetirementReceipt {
    pub fn received_through(&self) -> u64 {
        self.received_through
    }
}
