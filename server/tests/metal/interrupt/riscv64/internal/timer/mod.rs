use super::{Effect, Prior, ProgramStep, Programming, State};

#[test]
fn only_confirmed_deadlines_can_authorize_expiry() {
    let replacement = Programming {
        previous: Some(100),
        requested: 200,
        step: ProgramStep::Enable,
    };
    assert_eq!(State::Armed(100).deadline(), Some(100));
    for state in [
        State::Inherited(Some(100)),
        State::Programming(replacement),
        State::Uncertain(Effect::Programming(replacement)),
        State::AwaitingClear(Prior::Programming(replacement)),
    ] {
        assert_eq!(state.deadline(), None);
    }
    assert_eq!(State::Armed(200).deadline(), Some(200));
}
