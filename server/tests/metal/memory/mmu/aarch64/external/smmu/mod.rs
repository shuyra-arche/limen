use super::{Error, Profile, translation::Format};
use limen_treaty::model::PhysicalAddress;

#[test]
fn stage_two_requires_the_aarch64_format_and_4k_granule() {
    let profile = Profile {
        stage_two: true,
        granule_4k: true,
        enabled: false,
        aarch64_tables: true,
    };
    let format = Format::new(profile).expect("supported profile");
    assert_eq!(
        format.stream(PhysicalAddress::new(0x2000), 2).unwrap()[3],
        0x2000
    );
    assert_eq!(
        format.branch(PhysicalAddress::new(1 << 32)),
        Err(Error::Window)
    );
    for profile in [
        Profile {
            stage_two: false,
            ..profile
        },
        Profile {
            granule_4k: false,
            ..profile
        },
        Profile {
            aarch64_tables: false,
            ..profile
        },
    ] {
        assert!(matches!(Format::new(profile), Err(Error::Rejected)));
    }
}

#[test]
fn invalid_stream_geometry_and_occupied_vmid_do_not_alter_storage() {
    use super::{Binding, Context, DeviceOrder, Sequence, Storage, directory::Entry};
    use std::boxed::Box;

    let entries = Box::leak(Box::new([const { Entry::EMPTY }; 64]));
    let mut bindings = [None; 2];
    let mut sequence = Sequence::FRESH;
    let mut storage = Storage {
        entries,
        physical: PhysicalAddress::new(0x1000),
        bindings: &mut bindings,
        sequence: &mut sequence,
    };
    assert_eq!(storage.geometry(6), Ok(6));
    assert_eq!(storage.geometry(5), Err(Error::Rejected));
    storage.validate().unwrap();
    let format = Format::new(Profile {
        stage_two: true,
        granule_4k: true,
        enabled: false,
        aarch64_tables: true,
    })
    .unwrap();
    let original = Binding {
        controller: PhysicalAddress::new(0x2_0000),
        generation: storage.sequence.next().unwrap(),
        context: Context {
            stream: 48,
            root: PhysicalAddress::new(0x3000),
            vmid: 1,
        },
    };
    let (slot, words) = storage.entry(format, original.context).unwrap();
    let order = DeviceOrder(|| {});
    storage.publish(slot, original, words, &order);
    storage.block(original, &order);
    // Clearing validity still reserves the original entry and original VMID.
    let neighbour = Context {
        stream: 56,
        ..original.context
    };
    assert_eq!(storage.entry(format, original.context), Err(Error::Active));
    assert_eq!(storage.entry(format, neighbour), Err(Error::Active));
    assert_eq!(storage.binding(48), Some(original));
    assert_eq!(
        storage.entry(
            format,
            Context {
                stream: 64,
                ..neighbour
            }
        ),
        Err(Error::Rejected)
    );
    storage.retire(original, &order);
    assert_eq!(storage.binding(48), None);
    let replacement = Binding {
        generation: storage.sequence.next().unwrap(),
        ..original
    };
    let (slot, words) = storage.entry(format, replacement.context).unwrap();
    storage.publish(slot, replacement, words, &order);
    assert_ne!(replacement, original);
    assert_eq!(storage.binding(48), Some(replacement));
}
