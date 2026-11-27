//! Common use of one admitted absolute hardware comparator.

use crate::metal::chrono::Deadline;

pub trait Sealed {}

/// Arm a live native comparison and read whether its confirmed target is due.
///
/// The target must originate from the bound Chrono clock. Expiry is separate
/// from actual interrupt reception and its native completion. Each backend
/// retains exclusive comparator access and its own replacement and drain rules.
pub trait AbsoluteTimer: Sealed {
    type Failure;

    /// Start from a settled idle comparator and confirm the original target.
    fn arm_at(&mut self, target: Deadline<'_>) -> Result<(), Self::Failure>;

    fn due(&self) -> Result<bool, Self::Failure>;
}
