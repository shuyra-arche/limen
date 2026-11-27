//! Retirement of an exact I/O binding through its admitted controller.

pub trait Sealed {}

/// The result of polling the controller responsible for the original binding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Retirement {
    Pending,
    Settled,
}

/// Unbind a live requester on this actual remapping unit. The controller
/// keeps the original table entry and any unsettled operation after return.
pub trait Unbind: Sealed {
    type Binding: Copy;
    type Ticket;
    type Error;

    /// Start retirement of the original binding.
    ///
    /// # Safety
    /// The original endpoint is stopped and drained. Exclude endpoint
    /// translation caches and retain the original backing through retirement.
    unsafe fn retire(&mut self, binding: Self::Binding) -> Result<Self::Ticket, Self::Error>;

    /// Recover observation of the unsettled retirement for this exact binding.
    /// This does not retry a failed command or release the original tables.
    fn pending_retirement(&self, binding: Self::Binding) -> Option<Self::Ticket>;

    /// Poll precisely this retirement. The ticket grants observation only.
    fn observe_retirement(&mut self, ticket: &Self::Ticket) -> Result<Retirement, Self::Error>;
}
