use super::super::Controller;
use super::State;

#[test]
fn retained_claim_blocks_its_source_without_blocking_a_neighbor() {
    for state in [State::Received(22, 4), State::Awaiting(22, 4)] {
        assert!(state.retains_claim(4));
        assert!(!state.retains_claim(5));
        assert!(!state.excludes_source(4));
        assert!(state.excludes_source(5));
    }
    assert!(!State::Receiving(23).excludes_source(5));
    assert!(!State::Shutting.excludes_source(5));
}

#[test]
fn physical_domain_state_can_move_without_a_hart_local_mapping() {
    fn transferable<T: Send>() {}
    transferable::<Controller<'static, 'static>>();
}
