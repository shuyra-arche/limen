use super::{Error, Issuance, SendState};

#[test]
fn an_uncertain_issue_stays_with_its_original_destination() {
    let mut state = SendState::FRESH;
    assert_eq!(state.require_available(), Ok(()));
    assert_eq!(state.pending(5), Ok(None));

    state.retain_unconfirmed(5, -1);
    assert_eq!(state.require_available(), Err(Error::Busy));
    assert_eq!(state.pending(3), Err(Error::Busy));
    assert_eq!(
        state.pending(5),
        Ok(Some(Issuance::Unconfirmed(Error::Firmware(-1))))
    );
    assert_eq!(
        state.pending(5),
        Ok(Some(Issuance::Unconfirmed(Error::Firmware(-1))))
    );
}
