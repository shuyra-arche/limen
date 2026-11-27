use super::{Failure, Mode, State};

#[test]
fn retain_the_original_claim_across_split_effects() {
    let mut state = State::Empty;
    assert_eq!(state.finish_read(33), Err(Failure::Busy));
    state.begin_read().unwrap();
    assert_eq!(state.begin_read(), Err(Failure::Busy));
    assert_eq!(state.finish_read(33), Ok(Some(33)));
    assert_eq!(state.claim(), Some(33));
    assert_eq!(state.begin_priority_drop(34), Err(Failure::Busy));
    assert_eq!(state.begin_deactivation(33), Err(Failure::Busy));
    state.begin_priority_drop(33).unwrap();
    assert_eq!(state, State::DroppingPriority(33));
    assert_eq!(state.claim(), None);
    assert_eq!(state.begin_read(), Err(Failure::Busy));
    assert_eq!(
        state.finish_priority_drop(34, Mode::Split),
        Err(Failure::Busy)
    );
    state.finish_priority_drop(33, Mode::Split).unwrap();
    assert_eq!(state.claim(), Some(33));
    assert_eq!(state.begin_priority_drop(33), Err(Failure::Busy));
    state.begin_deactivation(33).unwrap();
    assert_eq!(state, State::Deactivating(33));
    assert_eq!(state.finish_deactivation(34), Err(Failure::Busy));
    assert_eq!(state.begin_read(), Err(Failure::Busy));
    state.finish_deactivation(33).unwrap();
    assert_eq!(state, State::Empty);
    assert_eq!(state.finish_deactivation(33), Err(Failure::Busy));
}

#[test]
fn special_reads_and_combined_or_lpi_eoi_leave_no_deactivation() {
    let mut state = State::Empty;
    for special in 1020..=1023 {
        state.begin_read().unwrap();
        let result = if special == 1023 {
            Ok(None)
        } else {
            Err(Failure::Special(special))
        };
        assert_eq!(state.finish_read(u64::from(special)), result);
        assert_eq!(state, State::Empty);
    }
    for (id, mode) in [(33, Mode::Combined), (8192, Mode::Split)] {
        state.begin_read().unwrap();
        assert_eq!(state.finish_read(u64::from(id)), Ok(Some(id)));
        state.begin_priority_drop(id).unwrap();
        state.finish_priority_drop(id, mode).unwrap();
        assert_eq!(state.begin_deactivation(id), Err(Failure::Busy));
    }
}
