//! Physical Hart descriptions and the local context installed at machine entry.

pub mod description;

#[cfg(any(limen_target, test))]
mod hardware;
#[cfg(all(limen_target, any(target_arch = "x86_64", target_arch = "aarch64")))]
pub(in crate::metal) use hardware::boot_identity;
#[cfg(any(limen_cooperative, test))]
pub(crate) use hardware::can_switch;
#[cfg(limen_target)]
pub(crate) use hardware::current_hart;
#[cfg(all(limen_target, target_arch = "aarch64"))]
pub use hardware::{Aarch64Cpus, CpuCandidate};
#[cfg(any(limen_target, test))]
pub(crate) use hardware::{
    Error, QueueNode, TURN_CAPACITY, TurnParticipation, begin_leaf, begin_turn,
};
#[cfg(limen_target)]
pub(in crate::metal) use hardware::{HartLocal, stack, stack_pointer};
#[cfg(all(limen_target, target_arch = "x86_64"))]
pub(in crate::metal) use hardware::{clear_local, matches_madt};

#[cfg(limen_target)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct HartId(usize);

#[cfg(limen_target)]
impl HartId {
    pub(crate) const fn new(value: usize) -> Self {
        Self(value)
    }

    pub(crate) const fn get(self) -> usize {
        self.0
    }
}
