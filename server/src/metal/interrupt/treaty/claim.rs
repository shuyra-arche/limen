//! Reception and completion on an already bound interrupt interface.

pub trait Sealed {}

/// An admitted local receiver that reads a hardware claim register. The
/// original context keeps the only open claim until completion is settled.
/// Architectures that accept vectors at Trap use that entry to bind their
/// claim and do not emulate a destructive register read.
pub trait Receive: Sealed {
    type Error;
    type Claim<'receiver>: HardwareClaim<Error = Self::Error>
    where
        Self: 'receiver;

    /// Read the original controller only if its previous claim has settled.
    fn receive(&mut self) -> Result<Option<Self::Claim<'_>>, Self::Error>;

    /// Reborrow the receiver's retained claim without reading hardware again.
    fn pending_claim(&mut self) -> Result<Option<Self::Claim<'_>>, Self::Error>;
}

/// Result of completing the exact claimed interrupt on the originating receiver.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Completion {
    Settled,
    /// The write was issued. The native owner still needs its original
    /// confirmation and must not reissue completion on this evidence.
    AwaitingConfirmation,
}

/// A borrowed native claim. Its owner retains the active record when the
/// borrow ends before completion. Acceptance of a CPU vector or a controller's
/// claim register belongs to the installed native receiver.
pub trait HardwareClaim: Sealed {
    type Error;

    /// Hardware source number within this original receiver's scope.
    fn source_number(&self) -> u32;

    /// Complete this claim with its original controller.
    ///
    /// # Safety
    /// The receiving context remains live and exclusive. The source has
    /// been serviced, masked, or otherwise made safe for completion, and
    /// any remaining software obligation is durably retained.
    unsafe fn finish(&mut self) -> Result<Completion, Self::Error>;
}
