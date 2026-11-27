//! Consume only the already acquired claim supplied by the real receiver.

use kernel::metal::interrupt::{Completion, HardwareClaim};

pub unsafe fn finish(claim: &mut impl HardwareClaim, expected: u32, completion: Completion) {
    assert_eq!(claim.source_number(), expected);
    assert!(unsafe { claim.finish() }.is_ok_and(|observed| observed == completion));
    assert!(unsafe { claim.finish() }.is_err());
}
