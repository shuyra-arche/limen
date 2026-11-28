use super::{Deadlines, Failure, Slot, TockId};
use core::num::NonZeroU64;

#[test]
fn due_members_do_not_hold_the_scheduled_minimum_or_block_each_other() {
    let mut deadlines = Deadlines::<_, 4>::new();
    let later = deadlines.arm(1, 30).unwrap();
    let first = deadlines.arm(2, 10).unwrap();
    let second = deadlines.arm(3, 10).unwrap();
    let next = deadlines.arm(4, 20).unwrap();
    assert_eq!(deadlines.earliest(), Some(10));
    assert_eq!(deadlines.discover_due(9, 4), 0);
    assert_eq!(deadlines.discover_due(10, 1), 1);
    assert_eq!(deadlines.earliest(), Some(10));
    assert_eq!(deadlines.due(), Some((first, 2, 10)));
    assert_eq!(deadlines.discover_due(20, 2), 2);
    assert_eq!(deadlines.earliest(), Some(30));

    deadlines.rotate_due(first, 2).unwrap();
    assert_eq!(deadlines.due(), Some((second, 3, 10)));
    assert_eq!(deadlines.retire_due(next, 9), Err(Failure::Gone));
    assert_eq!(deadlines.retire_due(later, 1), Err(Failure::NotDue));
    deadlines.retire_due(second, 3).unwrap();
    assert_eq!(deadlines.due(), Some((next, 4, 20)));
    deadlines.retire_due(next, 4).unwrap();
    assert_eq!(deadlines.due(), Some((first, 2, 10)));
    deadlines.retire_due(first, 2).unwrap();
    assert_eq!(deadlines.due(), None);
}

#[test]
fn exact_cancellation_repairs_the_heap_and_reuse_rejects_old_tock() {
    let mut deadlines = Deadlines::<_, 5>::new();
    let first = deadlines.arm(1, 10).unwrap();
    let high = deadlines.arm(2, 70).unwrap();
    let middle = deadlines.arm(3, 40).unwrap();
    let next = deadlines.arm(4, 20).unwrap();
    let following = deadlines.arm(5, 25).unwrap();
    assert_eq!(deadlines.arm(6, 15), Err(Failure::Capacity));
    assert_eq!(deadlines.unarm(next, 1), Err(Failure::Gone));
    deadlines.unarm(next, 4).unwrap();
    deadlines.unarm(first, 1).unwrap();
    deadlines.unarm(middle, 3).unwrap();
    assert_eq!(deadlines.earliest(), Some(25));
    let replacement = deadlines.arm(6, 15).unwrap();
    assert_eq!(replacement.slot, middle.slot);
    assert_ne!(replacement, middle);
    assert_eq!(deadlines.discover_due(100, 5), 3);
    assert_eq!(deadlines.unarm(replacement, 6), Err(Failure::AlreadyDue));
    assert_eq!(deadlines.check_due(middle, 6), Err(Failure::Gone));
    for (tock, occupancy, deadline) in [(replacement, 6, 15), (following, 5, 25), (high, 2, 70)] {
        assert_eq!(deadlines.due(), Some((tock, occupancy, deadline)));
        deadlines.retire_due(tock, occupancy).unwrap();
    }
    assert_eq!(deadlines.earliest(), None);
    assert_eq!(deadlines.due(), None);
}

#[test]
fn an_exhausted_generation_cannot_be_reissued() {
    let mut deadlines = Deadlines::<_, 1>::new();
    deadlines.slots[0] = Slot::Vacant {
        generation: NonZeroU64::MAX,
        next: None,
    };
    let original = deadlines.arm(1, 20).unwrap();
    assert_eq!(
        original,
        TockId {
            slot: 0,
            generation: NonZeroU64::MAX,
        }
    );
    assert_eq!(deadlines.discover_due(20, 1), 1);
    deadlines.retire_due(original, 1).unwrap();
    assert_eq!(deadlines.arm(2, 40), Err(Failure::Capacity));
    assert_eq!(deadlines.check_due(original, 1), Err(Failure::Gone));
}
