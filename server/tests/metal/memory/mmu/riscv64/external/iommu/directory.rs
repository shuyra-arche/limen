use super::super::{CommandIdentity, Effect, Refresh};
use super::{Binding, Context, Error, Sequence};
use limen_treaty::model::PhysicalAddress;

#[test]
fn a_prepared_binding_keeps_its_original_retirement_at_sequence_exhaustion() {
    let mut sequence = Sequence {
        number: u64::MAX - 2,
        ..Sequence::FRESH
    };
    let binding = Binding {
        controller: PhysicalAddress::new(0x1000),
        generation: sequence.prepare_binding().unwrap(),
        context: Context {
            device: 7,
            root: PhysicalAddress::new(0x3000),
            pscid: 1,
        },
    };
    assert_eq!(sequence.prepare_binding(), Err(Error::Exhausted));
    assert_eq!(sequence.next(), Ok(u64::MAX)); // First queue.
    assert_eq!(sequence.next_available(), Err(Error::Exhausted));
    assert_eq!(
        Effect::Refreshed(binding, Refresh::FirstStage).command_identity(&mut sequence),
        Err(Error::Exhausted),
    );
    assert_eq!(
        Effect::Unbound(binding).command_identity(&mut sequence),
        Ok(CommandIdentity::Retirement(u64::MAX - 1)),
    );
}

#[test]
fn final_binding_installation_and_its_retirement_have_disjoint_ids() {
    let mut sequence = Sequence {
        number: u64::MAX - 1,
        ..Sequence::FRESH
    };
    let binding = Binding {
        controller: PhysicalAddress::new(0x1000),
        generation: sequence.next().unwrap(),
        context: Context {
            device: 7,
            root: PhysicalAddress::new(0x3000),
            pscid: 1,
        },
    };
    assert_eq!(
        Effect::Bound(binding).command_identity(&mut sequence),
        Ok(CommandIdentity::Ordinary(u64::MAX)),
    );
    assert_eq!(
        Effect::Unbound(binding).command_identity(&mut sequence),
        Ok(CommandIdentity::Retirement(u64::MAX)),
    );
}
