use super::{ClearStep, Clearing, Effect, Event, Failure, Prior, ProgramStep, Programming, State};

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

#[test]
fn clear_resumes_observation_after_unconfirmed_programming() {
    let mut state = State::Armed(100);
    let program = Programming {
        previous: Some(100),
        requested: 80,
        step: ProgramStep::Compare,
    };
    state
        .advance(Event::Rearm {
            now: 20,
            target: 80,
        })
        .unwrap();
    assert_eq!(state, State::Programming(program));
    assert_eq!(state.advance(Event::Enabled(true)), Err(Failure::Busy));
    assert_eq!(
        state.advance(Event::Compared(Err(Failure::Firmware(-1)))),
        Err(Failure::Firmware(-1))
    );
    assert_eq!(state, State::Uncertain(Effect::Programming(program)));
    assert!(state.deadline().is_none());
    assert_eq!(
        state.advance(Event::Arm {
            now: 21,
            target: 81
        }),
        Err(Failure::Busy)
    );

    let prior = Prior::Programming(program);
    let clearing = Clearing {
        prior,
        step: ClearStep::Mask,
    };
    state.advance(Event::Shut).unwrap();
    assert_eq!(state, State::Clearing(clearing));
    assert_eq!(
        state.advance(Event::Masked(false)),
        Err(Failure::Unconfirmed)
    );
    assert_eq!(state, State::Uncertain(Effect::Clearing(clearing)));
    state.advance(Event::Shut).unwrap();
    state.advance(Event::Masked(true)).unwrap();
    assert_eq!(
        state.advance(Event::Compared(Err(Failure::Unconfirmed))),
        Err(Failure::Unconfirmed)
    );
    assert_eq!(
        state,
        State::Uncertain(Effect::Clearing(Clearing {
            prior,
            step: ClearStep::Compare,
        }))
    );
    state.advance(Event::Shut).unwrap();
    state.advance(Event::Masked(true)).unwrap();
    state.advance(Event::Compared(Ok(()))).unwrap();
    assert_eq!(state, State::AwaitingClear(prior));
    assert_eq!(state.advance(Event::Drained(false)), Err(Failure::Busy));
    state.advance(Event::Shut).unwrap();
    assert_eq!(state, State::AwaitingClear(prior));
    assert_eq!(state.advance(Event::Compared(Ok(()))), Err(Failure::Busy));
    state.advance(Event::Drained(true)).unwrap();
    assert_eq!(state, State::Idle);
}

#[test]
fn still_pending_enable_keeps_the_already_programmed_target() {
    let mut state = State::Idle;
    assert_eq!(
        state.advance(Event::Arm {
            now: 20,
            target: 20
        }),
        Err(Failure::Range)
    );
    state
        .advance(Event::Arm {
            now: 20,
            target: 60,
        })
        .unwrap();
    state.advance(Event::Compared(Ok(()))).unwrap();
    let program = Programming {
        previous: None,
        requested: 60,
        step: ProgramStep::Enable,
    };
    assert_eq!(state, State::Programming(program));
    assert_eq!(state.advance(Event::Compared(Ok(()))), Err(Failure::Busy));
    assert_eq!(
        state.advance(Event::Enabled(false)),
        Err(Failure::Unconfirmed)
    );
    assert_eq!(state, State::Uncertain(Effect::Programming(program)));
}
