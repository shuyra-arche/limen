use super::{Failure, State};

#[test]
fn a_live_or_expired_stop_is_retained_on_every_refused_replacement() {
    let mut state = State::Armed(100);
    assert_eq!(state.begin_program(Some(100), 20, 101), Err(Failure::Busy));
    assert_eq!(state.begin_program(Some(100), 100, 120), Err(Failure::Busy));
    assert_eq!(state.begin_stop(99), Err(Failure::Busy));
    assert_eq!(state, State::Armed(100));

    state.begin_program(Some(100), 20, 80).unwrap();
    let original = State::Programming {
        previous: Some(100),
        requested: 80,
    };
    assert_eq!(state.finish_program(81, true), Err(Failure::Unconfirmed));
    assert_eq!(state.finish_program(80, false), Err(Failure::Unconfirmed));
    assert_eq!(state, original);
    state.finish_program(80, true).unwrap();

    state.begin_stop(80).unwrap();
    assert_eq!(state.finish_stop(false), Err(Failure::Unconfirmed));
    assert_eq!(state, State::Masking(80));
    assert_eq!(state.begin_program(None, 81, 90), Err(Failure::Busy));
    state.finish_stop(true).unwrap();
    assert_eq!(state, State::Idle);
}
