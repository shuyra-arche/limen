//! Point-to-point hints for work the recipient can independently recheck.

pub trait Sealed {}

/// Evidence on the sending processor. The recipient alone observes delivery.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Issuance<I, E> {
    Confirmed(I),
    Pending(I),
    /// The original issue was attempted but the hardware has no further
    /// local evidence. The destination's obligations remain live.
    Unconfirmed(I, E),
}

impl<I: Copy, E> Issuance<I, E> {
    pub fn original(&self) -> I {
        match *self {
            Self::Confirmed(original)
            | Self::Pending(original)
            | Self::Unconfirmed(original, _) => original,
        }
    }
}

/// Exclusive use of an established remote notification route.
///
/// Hints can coalesce. Recipients recheck independently published work
/// after actual reception; a confirmed send is neither an interrupt claim
/// nor evidence that the recipient observed any work.
pub trait NotifyProcessor: Sealed {
    type Error;
    /// Exact original sending scope, destination and issue round. The shape
    /// is defined by the route's native mechanism.
    type Issue: Copy + Eq;

    /// Issue one hint after publishing the work. An error means rejection
    /// before issue. The sender retains a pending or unconfirmed issue.
    fn notify(&mut self) -> Result<Issuance<Self::Issue, Self::Error>, Self::Error>;

    /// Reacquire the original retained issue without issuing or confirming it.
    fn pending(&self) -> Result<Option<Self::Issue>, Self::Error>;

    /// Observe the retained local issue without sending again. Evidence from
    /// another issue is rejected, including after a later hint to this peer.
    fn confirm(
        &mut self,
        original: Self::Issue,
    ) -> Result<Issuance<Self::Issue, Self::Error>, Self::Error>;
}
