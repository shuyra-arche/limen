use super::super::{Limits, SourceSlot};
use super::{Failure, Priority, change_enable, enable, exclusive_source_slot};

#[test]
fn enable_update_preserves_other_sources_in_the_shared_register() {
    let (register, first) = enable(4, 10);
    let (same_register, second) = enable(4, 11);
    assert_eq!(register, same_register);
    assert_ne!(register, enable(5, 10).0);
    let both = change_enable(change_enable(0, first, true), second, true);
    assert_eq!(both, first | second);
    assert_eq!(change_enable(both, first, false), second);
    assert_eq!(change_enable(both, second, false), first);
}

#[test]
fn exclusive_source_admission_is_checked_before_allocating_a_route() {
    let first = SourceSlot(Some(Priority {
        id: 10,
        inherited: 1,
        requested: 2,
    }));
    let sources = [first, SourceSlot::EMPTY];
    assert_eq!(exclusive_source_slot(&sources, 10), Err(Failure::Busy));
    assert_eq!(exclusive_source_slot(&sources, 11), Ok(1));
    assert_eq!(
        exclusive_source_slot(&sources[..1], 11),
        Err(Failure::Capacity)
    );
}

#[test]
fn injected_limits_check_priority_and_threshold_independently() {
    let limits = Limits::new(3, 7).unwrap();
    assert!(Limits::new(0, 7).is_none());
    assert_eq!(limits.check_priority(0), Err(Failure::Scope));
    assert_eq!(limits.check_priority(4), Err(Failure::Scope));
    assert_eq!(limits.check_priority(3), Ok(()));
    assert_eq!(limits.check_threshold(7), Ok(()));
    assert_eq!(limits.check_threshold(8), Err(Failure::Scope));
}
