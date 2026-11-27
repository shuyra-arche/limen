use super::{Context, Directory, DirectoryWord, Error, Page, Profile, Table};
use limen_treaty::model::PhysicalAddress;

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

#[test]
fn cutoff_keeps_the_requester_and_its_slot_until_original_retirement() {
    use super::{Binding, DeviceOrder, Sequence};
    use std::boxed::Box;

    let profile = Profile {
        sv39: true,
        debug_translation: false,
        extended_context: true,
        little_endian_tables: true,
        address_bits: 48,
        directory: Directory::Off,
    };
    let page = Box::leak(Box::new(Page::EMPTY));
    let mut records = [None; 2];
    let mut sequence = Sequence::FRESH;
    let scope = PhysicalAddress::new(0x1_0000);
    let mut table = Table::admit(
        page,
        PhysicalAddress::new(0x1000),
        &mut records,
        profile,
        DirectoryWord(0),
    )
    .unwrap();
    let original = Binding::new(
        scope,
        sequence.next().unwrap(),
        Context {
            device: 56,
            root: PhysicalAddress::new(0x3000),
            pscid: 1,
        },
    );
    let order = DeviceOrder(|| {});
    let entry = table.entry(profile, original.context).unwrap();
    table.publish(entry, original, &order);
    assert_eq!(table.binding(56), Some(original));
    assert!(matches!(
        table.entry(
            profile,
            Context {
                device: 63,
                root: PhysicalAddress::new(0x5000),
                ..original.context
            }
        ),
        Err(Error::Mode)
    ));
    assert!(matches!(
        table.entry(
            profile,
            Context {
                device: 64,
                ..original.context
            }
        ),
        Err(Error::Address)
    ));
    table.block(profile, original, &order);
    assert_eq!(table.binding(56), Some(original));
    assert!(matches!(
        table.entry(profile, original.context),
        Err(Error::Mode)
    ));
    table.retire(profile, original, &order);
    assert_eq!(table.binding(56), None);
    let replacement = Binding {
        generation: sequence.next().unwrap(),
        ..original
    };
    let entry = table.entry(profile, replacement.context).unwrap();
    table.publish(entry, replacement, &order);
    assert_ne!(table.binding(56), Some(original));
    assert_eq!(table.binding(56), Some(replacement));
}
