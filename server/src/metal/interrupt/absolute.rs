//! Common use of one admitted absolute hardware comparator.

use crate::metal::chrono::Deadline;

pub trait Sealed {}

/// Program, advance and stop a live native absolute comparison.
///
/// The target must originate from the bound Chrono clock. Expiry is separate
/// from actual interrupt reception and its native completion. Each backend
/// retains exclusive comparator access and the original effects on failure.
pub trait AbsoluteTimer: Sealed {
    type Failure;

    /// Start from a settled idle comparator and confirm the original target.
    fn arm_at(&mut self, target: Deadline<'_>) -> Result<(), Self::Failure>;

    /// Set the active comparison to an equal or earlier future deadline.
    fn advance_deadline(&mut self, target: Deadline<'_>) -> Result<(), Self::Failure>;

    fn due(&self) -> Result<bool, Self::Failure>;

    /// Stop the original expired comparison. A separate hardware claim may
    /// still need completion through the receiver that actually acquired it.
    ///
    /// # Safety
    /// The original due obligation has been handled or durably retained.
    unsafe fn stop_due(&mut self) -> Result<(), Self::Failure>;
}
