use super::*;

fn instance(generation: Generation) -> CellId {
    CellId {
        slot: 1,
        generation,
    }
}

#[test]
fn prepared_access_is_not_an_accepted_access() {
    let cell = instance(Generation::FIRST);
    let range = ByteRange::new(ByteOffset::new(0), ByteOffset::new(4096)).unwrap();
    let (mut record, access) = AccessRecord::prepare(cell, range);
    let wrong = ByteRange::new(ByteOffset::new(0), ByteOffset::new(2048)).unwrap();

    assert_eq!(record.accepted(&access), Err(Error::Access));
    assert_eq!(record.accept(&access, wrong), Err(Error::Scope));
    assert_eq!(record.accepted(&access), Err(Error::Access));
    assert_eq!(record.accept(&access, range), Ok(()));
    assert_eq!(record.accepted(&access), Ok(()));
    assert_eq!(record.accept(&access, range), Err(Error::Access));
}

#[test]
fn reusing_the_cell_position_does_not_reuse_the_access_identity() {
    let first = instance(Generation::FIRST);
    let next = instance(Generation::next(Some(first.generation)).unwrap());
    let range = ByteRange::new(ByteOffset::new(0), ByteOffset::new(4096)).unwrap();
    let (mut old, original) = AccessRecord::prepare(first, range);
    old.accept(&original, range).unwrap();

    let (mut record, current) = AccessRecord::prepare(next, range);
    assert_ne!(current.id, original.id);
    assert_eq!(record.accept(&original, range), Err(Error::Scope));
    assert_eq!(record.accepted(&current), Err(Error::Access));
    record.accept(&current, range).unwrap();
    assert_eq!(record.accepted(&original), Err(Error::Scope));
    assert_eq!(record.accepted(&current), Ok(()));
}
