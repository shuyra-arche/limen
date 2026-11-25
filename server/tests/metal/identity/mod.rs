#![no_std]

use core::cell::Cell;

mod identity {
    include!("../../../src/metal/identity/mod.rs");
    include!("retirement.rs");
}

use identity::{Entry, GenerationTable, InitializeError, KeyError, Reservation, ReserveError};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Region {
    source: u32,
    index: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AllocationScope(Region);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ReturnScope(Region);

const REGION: Region = Region {
    source: 1,
    index: 3,
};

#[test]
fn only_published_entries_are_visible_and_reuse_changes_the_key() {
    let mut entries = [Entry::EMPTY];
    let mut table = GenerationTable::new(AllocationScope(REGION), &mut entries).unwrap();
    let reservation = table.reserve().unwrap();
    let key = reservation.key();
    assert_eq!(table.get(key), None);
    assert_eq!(table.get_mut(key), None);
    assert_eq!(table.detach(key), Err(KeyError::WrongState));

    assert_eq!(table.publish(reservation, 12).unwrap(), key);
    assert_eq!(table.capacity(), 1);
    assert_eq!(table.published_at(0), Some((key, &12)));
    assert_eq!(table.published_at(1), None);
    assert_eq!(table.get(key), Some(&12));
    *table.get_mut(key).unwrap() = 17;
    assert_eq!(table.reclaim(key).unwrap_err(), KeyError::WrongState);
    table.detach(key).unwrap();
    assert_eq!(table.get(key), None);
    assert_eq!(table.get_mut(key), None);
    assert_eq!(table.detach(key), Err(KeyError::WrongState));

    let (reclamation, value) = table.reclaim(key).unwrap();
    assert_eq!(value, 17);
    assert_eq!(reclamation.key(), key);
    assert_eq!(table.reclaim(key).unwrap_err(), KeyError::WrongState);
    assert_eq!(table.reserve().unwrap_err(), ReserveError::Capacity);
    table.release(reclamation).unwrap();

    let next = table.reserve().unwrap();
    assert_ne!(next.key(), key);
    assert_eq!(table.detach(key), Err(KeyError::Stale));
    assert_eq!(table.get(key), None);
    let reclamation = table.cancel(next).unwrap();
    table.release(reclamation).unwrap();
}

#[test]
fn cancellation_consumes_the_generation_and_waits_for_release() {
    let mut entries = [Entry::<()>::EMPTY];
    let mut table = GenerationTable::new(ReturnScope(REGION), &mut entries).unwrap();
    let preparation = table.reserve().unwrap();
    let first = preparation.key();
    let pending = table.cancel(preparation).unwrap();
    assert_eq!(pending.key(), first);
    assert_eq!(table.get(first), None);
    assert_eq!(table.reserve().unwrap_err(), ReserveError::Capacity);
    table.release(pending).unwrap();
    let second = table.reserve().unwrap();
    assert_ne!(first, second.key());
    assert_eq!(table.detach(first), Err(KeyError::Stale));
    let pending = table.cancel(second).unwrap();
    table.release(pending).unwrap();
}

#[derive(Debug)]
struct Value<'a>(&'a Cell<u32>);

impl Drop for Value<'_> {
    fn drop(&mut self) {
        self.0.set(self.0.get() + 1);
    }
}

#[test]
fn same_type_different_owners_reject_keys_and_return_linear_inputs() {
    let drops = Cell::new(0);
    let mut first_entries = [Entry::EMPTY];
    let mut second_entries = [Entry::EMPTY];
    let mut first = GenerationTable::new(AllocationScope(REGION), &mut first_entries).unwrap();
    let mut second = GenerationTable::new(
        AllocationScope(Region {
            source: 2,
            ..REGION
        }),
        &mut second_entries,
    )
    .unwrap();
    let left = first.reserve().unwrap();
    let right = second.reserve().unwrap();
    let key = left.key();
    assert_ne!(key, right.key());

    let (left, value, reason) = second.publish(left, Value(&drops)).unwrap_err();
    assert_eq!(reason, KeyError::WrongScope);
    assert_eq!(left.key(), key);
    assert_eq!(drops.get(), 0);
    let (left, reason) = second.cancel(left).unwrap_err();
    assert_eq!(reason, KeyError::WrongScope);
    first.publish(left, value).unwrap();
    assert!(second.get(key).is_none());
    assert!(second.get_mut(key).is_none());
    assert_eq!(second.detach(key), Err(KeyError::WrongScope));
    assert_eq!(second.reclaim(key).unwrap_err(), KeyError::WrongScope);

    first.detach(key).unwrap();
    let (left, value) = first.reclaim(key).unwrap();
    let (left, reason) = second.release(left).unwrap_err();
    assert_eq!(reason, KeyError::WrongScope);
    assert_eq!(left.key(), key);
    assert_eq!(drops.get(), 0);
    drop(value);
    first.release(left).unwrap();
    assert_eq!(drops.get(), 1);

    let right = second.cancel(right).unwrap();
    second.release(right).unwrap();
}

#[test]
fn batches_leave_all_slots_and_outputs_intact_when_preflight_fails() {
    let mut entries = [const { Entry::<()>::EMPTY }; 2];
    let mut table = GenerationTable::new(AllocationScope(REGION), &mut entries).unwrap();
    table.reserve_many(&mut []).unwrap();
    let mut output = [None, None, None];
    assert_eq!(table.reserve_many(&mut output), Err(ReserveError::Capacity));
    assert!(output.iter().all(Option::is_none));

    let first = table.reserve().unwrap();
    let key = first.key();
    assert_eq!(identity::issued_generation(key), 1);
    let mut short = [Some(first)];
    assert_eq!(
        table.reserve_many(&mut short),
        Err(ReserveError::OutputOccupied)
    );
    assert_eq!(short[0].as_ref().unwrap().key(), key);

    let mut oversized = [None, None];
    assert_eq!(
        table.reserve_many(&mut oversized),
        Err(ReserveError::Capacity)
    );
    assert!(oversized.iter().all(Option::is_none));
    let first = table.cancel(short[0].take().unwrap()).unwrap();
    table.release(first).unwrap();

    table.reserve_many(&mut oversized).unwrap();
    let [left, right] = oversized.map(Option::unwrap);
    let mut generations = [
        identity::issued_generation(left.key()),
        identity::issued_generation(right.key()),
    ];
    generations.sort_unstable();
    assert_eq!(generations, [1, 2]);
    let left = table.cancel(left).unwrap();
    let right = table.cancel(right).unwrap();
    table.release(left).unwrap();
    table.release(right).unwrap();
}

#[test]
fn region_preflights_both_typed_tables_before_reserving_any_slot() {
    let mut allocations = [const { Entry::<()>::EMPTY }; 2];
    let mut returns = [const { Entry::<()>::EMPTY }; 2];
    let mut allocation_table =
        GenerationTable::new(AllocationScope(REGION), &mut allocations).unwrap();
    let mut return_table = GenerationTable::new(ReturnScope(REGION), &mut returns).unwrap();
    let held_return = return_table.reserve().unwrap();
    let mut allocation_output: [Option<Reservation<AllocationScope>>; 2] = [None, None];
    let mut return_output: [Option<Reservation<ReturnScope>>; 2] = [None, None];

    let mut prepare = |allocations: &mut GenerationTable<'_, AllocationScope, ()>,
                       returns: &mut GenerationTable<'_, ReturnScope, ()>| {
        // Both real table borrows stay exclusive across the whole decision.
        allocations.check_reserve_many(&allocation_output)?;
        returns.check_reserve_many(&return_output)?;
        allocations
            .reserve_many(&mut allocation_output)
            .expect("preflight held");
        returns
            .reserve_many(&mut return_output)
            .expect("preflight held");
        Ok::<_, ReserveError>(())
    };
    assert_eq!(
        prepare(&mut allocation_table, &mut return_table),
        Err(ReserveError::Capacity)
    );
    assert_eq!(allocation_table.check_reserve(2), Ok(()));
    let held_return = return_table.cancel(held_return).unwrap();
    return_table.release(held_return).unwrap();
    prepare(&mut allocation_table, &mut return_table).unwrap();
    assert!(
        allocation_output
            .iter()
            .all(|token| { identity::issued_generation(token.as_ref().unwrap().key()) == 1 })
    );
    assert_eq!(
        allocation_table.reserve().unwrap_err(),
        ReserveError::Capacity
    );
    assert_eq!(return_table.reserve().unwrap_err(), ReserveError::Capacity);

    for token in allocation_output.map(Option::unwrap) {
        let reclamation = allocation_table.cancel(token).unwrap();
        allocation_table.release(reclamation).unwrap();
    }
    for token in return_output.map(Option::unwrap) {
        let reclamation = return_table.cancel(token).unwrap();
        return_table.release(reclamation).unwrap();
    }
}

#[test]
fn initialization_cannot_clear_slot_history() {
    let mut entries = [Entry::<()>::EMPTY];
    {
        let mut table = GenerationTable::new(AllocationScope(REGION), &mut entries).unwrap();
        let prepared = table.reserve().unwrap();
        let pending = table.cancel(prepared).unwrap();
        table.release(pending).unwrap();
    }
    assert_eq!(
        GenerationTable::new(AllocationScope(REGION), &mut entries).err(),
        Some(InitializeError::UsedStorage)
    );
    let mut empty: [Entry<()>; 0] = [];
    let mut table = GenerationTable::new(ReturnScope(REGION), &mut empty).unwrap();
    table.reserve_many(&mut []).unwrap();
    assert_eq!(table.reserve().unwrap_err(), ReserveError::Capacity);
}
