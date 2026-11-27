use super::{Setup, State};

#[test]
fn retirement_reuses_the_original_input_setup() {
    let setup = Setup {
        priority: 0x20,
        inherited_priority: 0xc0,
        group: false,
    };
    assert_eq!(State::Vacant.retirement(), None);
    assert_eq!(State::Configuring(setup).retirement(), None);
    for phase in [
        State::Masked(setup),
        State::Enabling(setup),
        State::Enabled(setup),
        State::Masking(setup),
        State::Restoring(setup),
    ] {
        assert_eq!(phase.retirement(), Some(setup));
    }
}
