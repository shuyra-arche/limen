//! Reception and completion on an already bound interrupt interface.

use limen_treaty::model::PhysicalAddress;

pub trait Sealed {}

/// Identity of an original claim in one continuously admitted receiver.
/// This observation does not grant access to a controller or its claim.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClaimId {
    controller: PhysicalAddress,
    context: u64,
    round: u64,
    source: u32,
}

impl ClaimId {
    pub(in crate::metal::interrupt) fn new(
        controller: PhysicalAddress,
        context: u64,
        round: u64,
        source: u32,
    ) -> Self {
        Self {
            controller,
            context,
            round,
            source,
        }
    }

    pub fn controller(self) -> PhysicalAddress {
        self.controller
    }

    pub fn context(self) -> u64 {
        self.context
    }

    pub fn round(self) -> u64 {
        self.round
    }

    pub fn source(self) -> u32 {
        self.source
    }
}

/// Evidence for the completion of one original claim.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClaimConfirmation {
    id: ClaimId,
    accepted: bool,
}

impl ClaimConfirmation {
    /// Read native confirmation of the retained effect.
    pub fn observe(id: ClaimId) -> Self {
        Self {
            id,
            accepted: false,
        }
    }

    /// Present qualified platform evidence for a receiver without native readback.
    ///
    /// # Safety
    /// The platform established that this exact issued completion reached and
    /// was accepted by the original controller context and source. It must
    /// have been observed in the same uninterrupted receiver admission.
    pub unsafe fn platform_accepted(id: ClaimId) -> Self {
        Self { id, accepted: true }
    }

    pub(in crate::metal::interrupt) fn id(self) -> ClaimId {
        self.id
    }

    pub(in crate::metal::interrupt) fn accepted(self) -> bool {
        self.accepted
    }
}

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

    /// Reborrow the receiver's retained claim without another destructive read.
    /// An unreadable claim result retains its reservation and returns an error.
    fn pending_claim(&mut self) -> Result<Option<Self::Claim<'_>>, Self::Error>;
}

/// Result of completing the exact claimed interrupt on the originating receiver.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Completion {
    Settled,
    /// The original completion needs further native observation.
    AwaitingObservation,
    /// The original write was issued. Its native interface has no readback,
    /// so this claim requires qualified acceptance from the platform.
    AwaitingAcceptance,
}

/// A borrowed native claim. Its owner retains the active record when the
/// borrow ends before completion. Acceptance of a CPU vector or a controller's
/// claim register belongs to the installed native receiver.
pub trait HardwareClaim: Sealed {
    type Error;

    /// Exact original controller, context, round and source.
    fn identity(&self) -> ClaimId;

    /// Hardware source number within this original receiver's scope.
    fn source_number(&self) -> u32 {
        self.identity().source()
    }

    /// Observe or confirm the original completion without issuing it again.
    /// Evidence for a different claim leaves the original claim unchanged.
    fn confirm(&mut self, evidence: ClaimConfirmation) -> Result<Completion, Self::Error>;

    /// Complete this claim with its original controller.
    ///
    /// # Safety
    /// The receiving context remains live and exclusive. The source has
    /// been serviced, masked, or otherwise made safe for completion, and
    /// any remaining software obligation is durably retained.
    unsafe fn finish(&mut self) -> Result<Completion, Self::Error>;
}
