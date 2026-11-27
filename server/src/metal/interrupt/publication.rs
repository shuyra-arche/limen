//! Publish an already prepared route through its original native owner.

pub trait Sealed {}

/// An exclusive borrow of the prepared source and its original destination.
/// The native owner keeps the route and any uncertain write when this borrow
/// ends. Preparation, masking and retirement remain native operations.
pub trait PublishRoute: Sealed {
    type Failure;

    /// Enable delivery from the original prepared route.
    ///
    /// # Safety
    /// The original receiver and source handler are ready before any
    /// delivery can occur and can retain an outstanding native claim.
    unsafe fn publish(&mut self) -> Result<(), Self::Failure>;
}
