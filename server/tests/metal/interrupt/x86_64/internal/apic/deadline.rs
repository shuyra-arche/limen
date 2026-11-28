use super::{Failure, State, TimerEffect};

#[test]
fn first_mode_and_original_write_are_separate_effects() {
    assert_eq!(State::Idle.arm(10, 10), Err(Failure::Busy));
    assert_eq!(State::Idle.arm(0, 0), Err(Failure::Busy));
    assert_eq!(State::Idle.arm(10, u64::MAX), Err(Failure::Busy));
    let selecting = State::Idle.arm(10, 30).unwrap();
    assert_eq!(
        selecting.effect(),
        Some(TimerEffect::Programming {
            previous: None,
            requested: 30,
        })
    );
    assert_eq!(selecting.confirmed(), Err(Failure::Busy));
    assert!(!selecting.received(40, 0));
    let issued = selecting.selected().unwrap();
    assert_eq!(issued.effect(), selecting.effect());
    assert_eq!(issued.selected(), Err(Failure::Busy));
    assert_eq!(issued.arm(10, 40), Err(Failure::Busy));
    assert_eq!(issued.observe(20, 30, false), Ok(State::Armed(30)));
}

#[test]
fn automatic_zero_needs_original_time_and_actual_native_delivery() {
    let issued = State::Idle.arm(10, 30).unwrap().selected().unwrap();
    assert_eq!(issued.observe(20, 0, true), Err(Failure::Unconfirmed));
    assert_eq!(issued.observe(30, 0, false), Err(Failure::Unconfirmed));
    assert_eq!(issued.observe(30, 40, true), Err(Failure::Unconfirmed));
    assert_eq!(issued.observe(30, 0, true), Ok(State::Armed(30)));
    assert!(!issued.received(29, 0));
    assert!(!issued.received(30, 30));
    assert!(issued.received(30, 0));
}

#[test]
fn advancement_never_postpones_or_replaces_an_expired_target() {
    let original = State::Armed(100);
    assert_eq!(original.confirmed(), Ok(Some(100)));
    assert_eq!(original.advance(10, 101), Err(Failure::Busy));
    assert_eq!(original.advance(100, 50), Err(Failure::Busy));
    assert_eq!(original.advance(50, 50), Err(Failure::Busy));
    assert_eq!(original.advance(10, 100), Ok(original));
    let earlier = original.advance(10, 80).unwrap();
    assert_eq!(
        earlier.effect(),
        Some(TimerEffect::Programming {
            previous: Some(100),
            requested: 80,
        })
    );
    assert_eq!(earlier.observe(50, 100, false), Err(Failure::Unconfirmed));
    let armed = earlier.observe(50, 80, false).unwrap();
    assert!(!armed.received(79, 0));
    assert!(!armed.received(81, 80));
    assert!(armed.received(81, 0));
    assert!(!State::Idle.received(100, 0));
    assert_eq!(State::Idle.confirmed(), Ok(None));
    assert_eq!(State::Idle.effect(), None);
}
