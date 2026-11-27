//! One Group 1 SGI admitted by the actual destination and sent to its affinity.

use crate::metal::processor::hart::aarch64::description::AFFINITY_MASK;

/// Encode ICC_SGI1R_EL1 with one destination and no Range Selector.
pub const fn selection(affinity: u64, intid: u8) -> Option<u64> {
    let aff0 = affinity & 0xff;
    if aff0 >= 16 || affinity & !AFFINITY_MASK != 0 || intid >= 16 {
        return None;
    }
    Some(
        (affinity & 0xff_0000_0000) << 16
            | (affinity & 0x00ff_0000) << 16
            | (intid as u64) << 24
            | (affinity & 0x0000_ff00) << 8
            | 1 << aff0,
    )
}

use core::marker::PhantomData;
use limen_treaty::model::PhysicalRange;

#[cfg_attr(
    not(all(limen_target, target_arch = "aarch64")),
    expect(
        dead_code,
        reason = "only a native SGI operation can produce these phases"
    )
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Phase {
    Prepared,
    Sending,
    Sent,
    Settled,
}

/// Linear qualification carried between source and destination. Its type
/// retains the target's original backing and gives no access by address.
#[cfg_attr(
    not(all(limen_target, target_arch = "aarch64")),
    expect(
        dead_code,
        reason = "native delivery and retirement use the reserved register selection and original scope"
    )
)]
#[derive(Debug)]
#[must_use]
pub struct Sgi<'map> {
    pub(super) physical: PhysicalRange,
    pub(super) domain: PhysicalRange,
    pub(super) id: u8,
    pub(super) address: u64,
    pub(super) phase: Phase,
    pub(super) mapping: PhantomData<&'map mut ()>,
}

impl Sgi<'_> {
    pub fn intid(&self) -> u8 {
        self.id
    }
}

#[cfg(test)]
#[path = "../../../../../tests/metal/interrupt/aarch64/gic/sgi.rs"]
mod tests;
