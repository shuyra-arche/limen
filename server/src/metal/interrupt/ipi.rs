//! Point-to-point hints for work the recipient can independently recheck.

pub trait Sealed {}

/// Evidence on the sending Hart. The recipient alone observes delivery.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Issuance<E> {
    Confirmed,
    Pending,
    /// The original issue was attempted but the hardware has no further
    /// local evidence. The destination's obligations remain live.
    Unconfirmed(E),
}

/// Exclusive use of an established remote notification route.
///
/// Hints can coalesce. Recipients recheck independently published work
/// after actual reception; a confirmed send is neither an interrupt claim
/// nor evidence that the recipient observed any work.
pub trait NotifyHart: Sealed {
    type Error;

    /// Issue one hint after publishing the work. An error means rejection
    /// before issue. The sender retains a pending or unconfirmed issue.
    fn notify(&mut self) -> Result<Issuance<Self::Error>, Self::Error>;

    /// Observe the original retained issue without sending it again.
    /// Return `None` when the bound route has no retained issue.
    fn pending(&mut self) -> Result<Option<Issuance<Self::Error>>, Self::Error>;
}
