use super::{Directory, DirectoryWord, Error};
use limen_treaty::model::PhysicalAddress;

#[test]
fn directory_readback_checks_the_installed_root() {
    let previous = DirectoryWord((1 << 54) | 1);
    let target = match previous.one(PhysicalAddress::new(0x9ab_000), 48) {
        Ok(target) => target,
        Err(error) => panic!("valid directory address: {error:?}"),
    };
    assert_eq!(target.mode(), Directory::One);
    assert_eq!(target.root(), Some(PhysicalAddress::new(0x9ab_000)));
    assert_eq!(target.0 >> 54, previous.0 >> 54);
    assert!(!DirectoryWord(target.0 ^ (1 << 10)).confirms(target));
    assert!(target.confirms(target));

    let off = target.off();
    assert_eq!(off.mode(), Directory::Off);
    assert_eq!(off.0 & !0xf, target.0 & !0xf);
    assert!(DirectoryWord(off.0 ^ (1 << 10)).confirms(off));
    assert!(!off.confirms(target));
}

#[test]
fn directory_rejects_unaligned_and_out_of_range_roots() {
    let previous = DirectoryWord(0);
    assert!(matches!(
        previous.one(PhysicalAddress::new(0x123), 48),
        Err(Error::Address)
    ));
    assert!(matches!(
        previous.one(PhysicalAddress::new(1 << 38), 38),
        Err(Error::Address)
    ));
    assert!(matches!(
        previous.one(PhysicalAddress::new(1 << 56), 64),
        Err(Error::Address)
    ));
}

#[test]
fn cutoff_keeps_the_requester_and_its_slot_until_original_retirement() {
    use super::{Binding, Context, DeviceOrder, Profile, Sequence, Storage, directory::Page};
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
    let mut storage = Storage {
        page,
        physical: PhysicalAddress::new(0x1000),
        bindings: &mut records,
        sequence: &mut sequence,
    };
    storage.validate(profile).unwrap();
    let original = Binding {
        controller: scope,
        generation: storage.sequence.next().unwrap(),
        context: Context {
            device: 56,
            root: PhysicalAddress::new(0x3000),
            pscid: 1,
        },
    };
    let order = DeviceOrder(|| {});
    let (slot, words) = storage.entry(profile, original.context).unwrap();
    storage.publish(profile, slot, original, words, &order);
    assert_eq!(storage.binding(56), Some(original));
    assert!(matches!(
        storage.entry(
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
        storage.entry(
            profile,
            Context {
                device: 64,
                ..original.context
            }
        ),
        Err(Error::Address)
    ));
    storage.block(profile, original, &order);
    assert_eq!(storage.binding(56), Some(original));
    assert!(matches!(
        storage.entry(profile, original.context),
        Err(Error::Mode)
    ));
    storage.retire(profile, original, &order);
    assert_eq!(storage.binding(56), None);
    let replacement = Binding {
        generation: storage.sequence.next().unwrap(),
        ..original
    };
    let (slot, words) = storage.entry(profile, replacement.context).unwrap();
    storage.publish(profile, slot, replacement, words, &order);
    assert_ne!(storage.binding(56), Some(original));
    assert_eq!(storage.binding(56), Some(replacement));
}

#[test]
fn sequence_cannot_move_to_another_register_scope() {
    use super::Sequence;
    use limen_treaty::model::ByteLength;
    let mut sequence = Sequence::FRESH;
    let bytes = ByteLength::new(4096);
    let first = PhysicalAddress::new(0x1000).range(bytes).unwrap();
    let other = PhysicalAddress::new(0x2000).range(bytes).unwrap();
    sequence.claim(first).unwrap();
    let old = sequence.next().unwrap();
    assert_eq!(sequence.claim(other), Err(Error::Address));
    sequence.claim(first).unwrap();
    assert!(sequence.next().unwrap() > old);
}
