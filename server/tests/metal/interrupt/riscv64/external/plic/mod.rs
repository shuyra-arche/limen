use super::{Controller, State, change_enable, enable};

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
fn retained_claim_blocks_its_source_without_blocking_a_neighbor() {
    for state in [State::Received(22, 4), State::Awaiting(22, 4)] {
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
