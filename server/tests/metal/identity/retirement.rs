use super::super::{
    Entry, Generation, GenerationTable, Key, KeyError, Reclamation, Reservation, ReserveError,
    Slot, State,
};
use core::num::NonZeroU32;

pub(super) fn issued_generation<Scope>(key: Key<Scope>) -> u32 {
    key.generation.0.get()
}

#[test]
fn maximum_generation_can_settle_while_the_rest_of_the_table_is_full() {
    let mut entries = [const { Entry::EMPTY }; 2];
    let mut table = GenerationTable::new((), &mut entries).unwrap();
    // Start two legitimate free slots immediately below their last issue.
    let previous = Some(Generation(NonZeroU32::new(u32::MAX - 1).unwrap()));
    table.entries[0].generation = previous;
    table.entries[1].generation = previous;
    let first = table.reserve().unwrap();
    let second = table.reserve().unwrap();
    assert_eq!(first.key().generation.0.get(), u32::MAX);
    assert_eq!(second.key().generation.0.get(), u32::MAX);
    assert_eq!(table.reserve().unwrap_err(), ReserveError::Capacity);

    let published = table.publish(first, 8).unwrap();
    table.detach(published).unwrap();
    let (reclamation, original) = table.reclaim(published).unwrap();
    assert_eq!(original, 8);
    table.release(reclamation).unwrap();
    assert_eq!(table.reserve().unwrap_err(), ReserveError::Capacity);

    let reclamation = table.cancel(second).unwrap();
    table.release(reclamation).unwrap();
    assert_eq!(
        table.reserve().unwrap_err(),
        ReserveError::IdentityExhausted
    );
    assert_eq!(table.check_reserve(1), Err(ReserveError::IdentityExhausted));
    assert_eq!(table.check_reserve(0), Ok(()));
    assert!(
        table
            .entries
            .iter()
            .all(|entry| matches!(entry.state, State::Retired))
    );
}

#[test]
fn stale_and_outside_keys_cannot_change_the_current_entry() {
    let mut entries = [Entry::EMPTY];
    let mut table = GenerationTable::new((), &mut entries).unwrap();
    let first = table.reserve().unwrap();
    let old_key = first.key();
    let pending = table.cancel(first).unwrap();
    table.release(pending).unwrap();
    let second = table.reserve().unwrap();
    let second_key = table.publish(second, 9).unwrap();

    let outside = Key {
        slot: Slot(1),
        ..second_key
    };
    assert_eq!(table.detach(outside), Err(KeyError::OutsideTable));
    assert_eq!(table.get(outside), None);
    assert_eq!(table.detach(old_key), Err(KeyError::Stale));
    assert_eq!(table.get(old_key), None);
    // Inject a replayed internal token. Safe callers cannot construct this.
    let (late, reason) = table.release(Reclamation(old_key)).unwrap_err();
    assert_eq!(reason, KeyError::Stale);
    assert_eq!(late.key(), old_key);
    let (duplicate, value, reason) = table.publish(Reservation(second_key), 1).unwrap_err();
    assert_eq!(reason, KeyError::WrongState);
    assert_eq!(duplicate.key(), second_key);
    assert_eq!(value, 1);
    assert_eq!(table.get(second_key), Some(&9));
    table.detach(second_key).unwrap();
    let (actual, value) = table.reclaim(second_key).unwrap();
    assert_eq!(value, 9);
    table.release(actual).unwrap();
}
