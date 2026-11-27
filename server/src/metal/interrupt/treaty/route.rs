//! Control an already prepared route through its original native owner.

pub trait Sealed {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RouteOperation {
    Publish,
    Mask,
    Resume,
    Retire,
}

/// Progress on the original physical route, as recorded by its native owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RouteState {
    Prepared,
    Open,
    Masked,
    /// An effect may have occurred. Reacquire the same route to continue
    /// the recorded operation. A mask may also contain an uncertain enable.
    Unconfirmed(RouteOperation),
    Retired,
}

/// An exclusive borrow of the prepared source and its original destination.
/// The native owner keeps the route and any uncertain write when this borrow
/// ends. An interrupt already taken retains its original completion path.
pub trait RouteControl: Sealed {
    type Failure;
    /// Resources returned by the original owner when retirement is settled.
    type Retired;

    /// Observe progress without acknowledging or writing to the controller.
    fn state(&self) -> RouteState;

    /// Enable delivery from the original prepared route.
    ///
    /// # Safety
    /// The original receiver and source handler are ready before any
    /// delivery can occur and can retain an outstanding native claim.
    unsafe fn publish(&mut self) -> Result<(), Self::Failure>;

    /// Mask forwarding from this open route while retaining its native binding
    /// and the resources for completing any original claim.
    fn mask(&mut self) -> Result<(), Self::Failure>;

    /// Restore delivery on the original masked route.
    ///
    /// # Safety
    /// The original receiver and source handler are ready for new and
    /// pending deliveries and can retain an outstanding native claim.
    unsafe fn resume(&mut self) -> Result<(), Self::Failure>;

    /// Close the original route and return its native retirement resources.
    ///
    /// # Safety
    /// The original source owner must have stopped new assertions and
    /// established that device, gateway and interconnect effects cannot
    /// produce further old deliveries or writes. All receivers must have
    /// settled the original claims and associated delivery obligations.
    unsafe fn retire(&mut self) -> Result<Self::Retired, Self::Failure>;
}
