use super::{Error, PhysicalAddress, Sequence};

#[test]
fn a_prepared_binding_leaves_one_queue_identity_at_sequence_exhaustion() {
    let mut sequence = Sequence {
        number: u64::MAX - 2,
        ..Sequence::FRESH
    };
    assert_eq!(sequence.prepare_binding(), Ok(u64::MAX - 1));
    assert_eq!(sequence.prepare_binding(), Err(Error::Exhausted));
    assert_eq!(sequence.next(), Ok(u64::MAX));
    assert_eq!(sequence.next_available(), Err(Error::Exhausted));
}

#[test]
fn sequence_cannot_move_to_another_register_scope() {
    use limen_treaty::model::ByteLength;
    let mut sequence = Sequence::FRESH;
    let bytes = ByteLength::new(4096);
    let first = PhysicalAddress::new(0x1000).range(bytes).unwrap();
    let other = PhysicalAddress::new(0x2000).range(bytes).unwrap();
    sequence.claim(first).unwrap();
    let old = sequence.next().unwrap();
    assert_eq!(sequence.claim(other), Err(Error::Address));
    sequence.claim(first).unwrap();
    assert!(sequence.next().unwrap() > old);
}
