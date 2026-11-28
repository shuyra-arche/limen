//! Common use of one admitted absolute hardware comparator.

use crate::metal::chrono::treaty::Deadline;

pub trait Sealed {}

/// Original effect retained by this bound comparator after an uncertain result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimerEffect {
    /// Native counts belong to this comparator's original Chrono clock.
    Programming {
        previous: Option<u64>,
        requested: u64,
    },
    Clearing,
}

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

    /// Report the last confirmed native target, or an admitted idle comparator.
    /// Inherited or unsettled programming is an error.
    fn confirmed_deadline(&self) -> Result<Option<Deadline<'_>>, Self::Failure>;

    fn due(&self) -> Result<bool, Self::Failure>;

    /// Inspect the original uncertain effect held by this comparator.
    fn pending_effect(&self) -> Result<Option<TimerEffect>, Self::Failure>;

    /// Observe the retained effect and progress its remaining steps. This
    /// never repeats an issued write. The owner retains unobservable effects.
    fn confirm_effect(&mut self) -> Result<(), Self::Failure>;

    /// Clear the admitted comparison, including any inherited programming.
    /// A separate receiver may retain a native claim after the stop. A
    /// backend may require expiry before clearing its active comparison.
    ///
    /// # Safety
    /// Responsibility for the original stop and wake obligations has settled
    /// or is continuously covered elsewhere. Any native claim remains held
    /// by its original receiver.
    unsafe fn clear(&mut self) -> Result<(), Self::Failure>;

    /// Stop the original expired comparison. A separate hardware claim may
    /// still need completion through the receiver that actually acquired it.
    ///
    /// # Safety
    /// The original due obligation has been handled or durably retained.
    unsafe fn stop_due(&mut self) -> Result<(), Self::Failure>;
}
