use super::{Error, SendState};

#[test]
fn an_uncertain_issue_stays_with_its_original_destination_and_round() {
    let mut state = SendState::FRESH;
    assert_eq!(state.pending(5), Ok(None));

    let previous = state.begin(5).unwrap();
    state.finish(Ok(()));
    assert_eq!(state.pending(5), Ok(None));

    let original = state.begin(5).unwrap();
    assert!(original.round > previous.round);
    assert_eq!(state.pending(5), Ok(Some(original)));
    assert_eq!(state.begin(5), Err(Error::Busy));
    state.finish(Err(-1));
    assert_eq!(state.begin(5), Err(Error::Busy));
    assert_eq!(state.pending(3), Err(Error::Busy));
    let retained = state.pending(5).unwrap().unwrap();
    assert_eq!(retained.hart, original.hart);
    assert_eq!(retained.round, original.round);
    assert_eq!(retained.error, Some(-1));
    assert_eq!(state.pending(5), Ok(Some(retained)));
}
