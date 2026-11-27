use super::{Failure, Reception};

#[test]
fn accepted_vector_survives_exhausted_claim_rounds() {
    let mut round = u64::MAX - 1;
    let mut reception = Reception::Idle;
    assert_eq!(reception.record(&mut round, 0xf0), Ok(()));
    assert_eq!(round, u64::MAX);
    assert_eq!(reception, Reception::Received(0xf0));

    // The last claim settled, but another already-routed vector entered.
    reception = Reception::Idle;
    assert_eq!(reception.record(&mut round, 0xf1), Err(Failure::Busy));
    assert_eq!(round, u64::MAX);
    assert_eq!(reception, Reception::Exhausted(0xf1));
    assert!(reception.involves(0xf1));
    assert!(!reception.involves(0xf0));
}
