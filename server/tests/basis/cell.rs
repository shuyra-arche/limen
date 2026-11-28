use super::*;

fn instance(generation: Generation) -> CellId {
    CellId {
        slot: 1,
        generation,
    }
}

fn access_id(cell: CellId) -> AccessId {
    AccessId(Key::new(cell, Slot::new(0).unwrap(), Generation::FIRST))
}

#[test]
fn prepared_access_is_not_an_accepted_access() {
    let cell = instance(Generation::FIRST);
    let range = ByteRange::new(ByteOffset::new(0), ByteOffset::new(4096)).unwrap();
    let owner = AccessOwner::Preparation(1);
    let rights = Rights::READ_WRITE;
    let (mut record, access) = AccessRecord::prepare(access_id(cell), range, rights, owner);
    let wrong = ByteRange::new(ByteOffset::new(0), ByteOffset::new(2048)).unwrap();

    assert_eq!(record.accepted(&access), Err(Error::Access));
    assert_eq!(
        record.accept(&access, wrong, rights, owner),
        Err(Error::Scope)
    );
    assert_eq!(
        record.accept(&access, range, Rights::READ, owner),
        Err(Error::Scope)
    );
    assert_eq!(
        record.accept(&access, range, rights, AccessOwner::Preparation(2)),
        Err(Error::Scope)
    );
    assert_eq!(record.accepted(&access), Err(Error::Access));
    assert_eq!(record.accept(&access, range, rights, owner), Ok(()));
    assert_eq!(record.accepted(&access), Ok(()));
    assert_eq!(
        record.accept(&access, range, rights, owner),
        Err(Error::Access)
    );
}

#[test]
fn reusing_the_cell_position_does_not_reuse_the_access_identity() {
    let first = instance(Generation::FIRST);
    let next = instance(Generation::next(Some(first.generation)).unwrap());
    let range = ByteRange::new(ByteOffset::new(0), ByteOffset::new(4096)).unwrap();
    let owner = AccessOwner::Preparation(1);
    let rights = Rights::READ_WRITE;
    let (mut old, original) = AccessRecord::prepare(access_id(first), range, rights, owner);
    old.accept(&original, range, rights, owner).unwrap();

    let (mut record, current) = AccessRecord::prepare(access_id(next), range, rights, owner);
    assert_ne!(current.id, original.id);
    assert_eq!(
        record.accept(&original, range, rights, owner),
        Err(Error::Scope)
    );
    assert_eq!(record.accepted(&current), Err(Error::Access));
    record.accept(&current, range, rights, owner).unwrap();
    assert_eq!(record.accepted(&original), Err(Error::Scope));
    assert_eq!(record.accepted(&current), Ok(()));
}
