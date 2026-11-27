use super::super::{CommandIdentity, Directory, Effect, Profile, Refresh};
use super::{Binding, Context, Error, Page, Sequence};
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

#[test]
fn basic_and_extended_contexts_use_their_own_directory_boundaries() {
    let basic = Profile {
        sv39: true,
        debug_translation: false,
        extended_context: false,
        little_endian_tables: true,
        address_bits: 56,
        directory: Directory::One,
    };
    let extended = Profile {
        extended_context: true,
        ..basic
    };
    for (profile, capacity, last_context) in [(basic, 128, 508..512), (extended, 64, 504..512)] {
        assert_eq!(Page::capacity(profile), capacity);
        assert_eq!(
            Page::context_words(profile, capacity as u32 - 1),
            Ok(last_context)
        );
        assert_eq!(
            Page::context_words(profile, capacity as u32),
            Err(Error::Address)
        );
    }
    let context = Context {
        device: 63,
        root: PhysicalAddress::new(0x3000),
        pscid: 0x12345,
    };
    let expected = [1, 0, 0x12345_000, (8 << 60) | 3];
    assert_eq!(context.words(basic), Ok(expected));
    assert_eq!(context.words(extended), Ok(expected));
}
