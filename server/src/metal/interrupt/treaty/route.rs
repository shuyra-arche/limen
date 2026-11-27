//! Control an already prepared route through its original native owner.

pub trait Sealed {}

/// An exclusive borrow of the prepared source and its original destination.
/// The native owner keeps the route and any uncertain write when this borrow
/// ends. An interrupt already taken retains its original completion path.
pub trait RouteControl: Sealed {
    type Failure;

    /// Enable delivery from the original prepared route.
    ///
    /// # Safety
    /// The original receiver and source handler are ready before any
    /// delivery can occur and can retain an outstanding native claim.
    unsafe fn publish(&mut self) -> Result<(), Self::Failure>;

    /// Mask forwarding from this route while retaining its native binding
    /// and the resources for completing any original claim.
    fn mask(&mut self) -> Result<(), Self::Failure>;

    /// Close the original route and release its reserved storage.
    ///
    /// # Safety
    /// The original source owner must have stopped new assertions and
    /// established that device, gateway and interconnect effects cannot
    /// produce further old deliveries or writes. All receivers must have
    /// settled the original claims and associated delivery obligations.
    unsafe fn retire(&mut self) -> Result<(), Self::Failure>;
}
