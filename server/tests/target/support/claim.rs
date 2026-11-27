//! Progress actual reception using the bound receiver and retained claim.

use kernel::metal::interrupt::treaty::{ClaimId, HardwareClaim, Receive};

pub fn receive<R: Receive>(receiver: &mut R, expected: u32) -> Option<R::Claim<'_>> {
    let claim = receiver
        .receive()
        .ok()
        .expect("original receiver accepts the read")?;
    assert_eq!(claim.source_number(), expected);
    Some(claim)
}

pub fn pending<R: Receive>(receiver: &mut R, expected: ClaimId) -> R::Claim<'_> {
    assert!(receiver.receive().is_err());
    let claim = receiver
        .pending_claim()
        .ok()
        .flatten()
        .expect("retained original claim");
    assert_eq!(claim.identity(), expected);
    claim
}
