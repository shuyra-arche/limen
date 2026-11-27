#[test]
fn invalid_stream_geometry_and_occupied_vmid_do_not_alter_storage() {
    use super::super::{
        Profile, Sequence,
        directory::{Directory, Entry},
    };
    use super::{Binding, Bindings, Context, DeviceOrder, Error, Format, PhysicalAddress};
    use std::boxed::Box;

    let entries = Box::leak(Box::new([const { Entry::EMPTY }; 64]));
    let mut bindings = [None; 2];
    let mut sequence = Sequence::FRESH;
    let physical = PhysicalAddress::new(0x1000);
    let too_narrow = Box::leak(Box::new([const { Entry::EMPTY }; 64]));
    assert!(matches!(
        Directory::new(too_narrow, physical, 5),
        Err(Error::Rejected)
    ));
    let directory = Directory::new(entries, physical, 6).unwrap();
    assert_eq!(directory.table().bits, 6);
    let mut storage = Bindings::new(directory, &mut bindings).unwrap();
    let format = Format::new(Profile {
        stage_two: true,
        granule_4k: true,
        enabled: false,
        aarch64_tables: true,
    })
    .unwrap();
    let original = Binding {
        controller: PhysicalAddress::new(0x2_0000),
        generation: sequence.next().unwrap(),
        context: Context {
            stream: 48,
            root: PhysicalAddress::new(0x3000),
            vmid: 1,
        },
    };
    let (slot, words) = storage.prepare(format, original.context).unwrap();
    let order = DeviceOrder(|| {});
    storage.publish(slot, original, words, &order);
    storage.block(original, &order);
    // Clearing validity still reserves the original entry and original VMID.
    let neighbour = Context {
        stream: 56,
        ..original.context
    };
    assert_eq!(
        storage.prepare(format, original.context),
        Err(Error::Active)
    );
    assert_eq!(storage.prepare(format, neighbour), Err(Error::Active));
    assert_eq!(storage.lookup(48), Some(original));
    assert_eq!(
        storage.prepare(
            format,
            Context {
                stream: 64,
                ..neighbour
            }
        ),
        Err(Error::Rejected)
    );
    storage.retire(original, &order);
    assert_eq!(storage.lookup(48), None);
    let replacement = Binding {
        generation: sequence.next().unwrap(),
        ..original
    };
    let (slot, words) = storage.prepare(format, replacement.context).unwrap();
    storage.publish(slot, replacement, words, &order);
    assert_ne!(replacement, original);
    assert_eq!(storage.lookup(48), Some(replacement));
}
