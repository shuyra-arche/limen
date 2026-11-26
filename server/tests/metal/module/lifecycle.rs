use super::{IMAGES, PAGE};
use crate::{
    access::MappedRange,
    address::PhysicalAddress,
    module::{Contract, Layout, ModuleImage, ModuleSlots, UseError},
};
use std::{mem::MaybeUninit, sync::Barrier, thread};

const GEOMETRY: Contract = Contract::MmuGeometry;

fn image() -> ModuleImage<'static> {
    let (machine, bytes) = IMAGES[1];
    ModuleImage::read(bytes, machine, PAGE).unwrap()
}

fn prepare(
    image: &ModuleImage<'static>,
    physical: u64,
    virtual_start: u64,
) -> (Layout<'static>, Box<[MaybeUninit<u8>]>) {
    let length = image.required_length();
    let physical = PhysicalAddress::new(physical).range(length).unwrap();
    let mut storage = vec![MaybeUninit::uninit(); length.as_usize().unwrap()].into_boxed_slice();
    let layout = {
        // The synthetic physical range represents this exclusively owned
        // simulation buffer. No target address is invoked on the Host.
        let mut mapped = unsafe { MappedRange::new(physical, &mut storage) }.unwrap();
        image.prepare(&mut mapped, virtual_start).unwrap()
    };
    (layout, storage)
}

#[test]
fn shutting_denies_admission_and_retirement_requires_an_effect_witness() {
    let image = image();
    let (layout, backing) = prepare(&image, 0x20_0000, 0x4000_0000);
    let entry = layout.entry(GEOMETRY).unwrap();
    let original_backing = backing.as_ptr();
    let slots = ModuleSlots::<1>::new();
    // The Host models settled publication by keeping the initialized buffer
    // with its owner. No actual target instruction or mapping is installed.
    let module = unsafe { slots.publish(layout, backing, []) }.ok().unwrap();
    assert!(matches!(module.retire(), Err(UseError::Open)));
    let first = module.acquire(GEOMETRY, 1).unwrap();
    let second = module.acquire(GEOMETRY, 1).unwrap();
    assert_eq!(first.id(), module.id());
    assert_eq!(first.entry(), entry);
    assert!(matches!(
        module.acquire(GEOMETRY, 2),
        Err(UseError::Interface)
    ));

    module.shut();
    module.shut();
    assert!(matches!(module.acquire(GEOMETRY, 1), Err(UseError::Closed)));
    assert!(matches!(module.retire(), Err(UseError::Busy)));
    assert_eq!(first.entry(), entry);
    drop(first);
    assert!(matches!(module.retire(), Err(UseError::Busy)));
    drop(second);

    let attempt = module.retire().unwrap();
    assert!(matches!(module.retire(), Err(UseError::Busy)));
    // An abandoned attempt must leave the original obligations in place.
    drop(attempt);
    let module = module.reclaim().err().unwrap();
    assert!(matches!(module.acquire(GEOMETRY, 1), Err(UseError::Closed)));
    let attempt = module.retire().unwrap();
    // The Host installed no target mapping and never executed the module.
    unsafe { attempt.complete() };
    assert!(matches!(module.retire(), Err(UseError::Retired)));
    let backing = module.reclaim().ok().unwrap();
    assert_eq!(backing.as_ptr(), original_backing);
}

#[test]
fn a_racing_admission_is_closed_or_counted_before_retirement() {
    let image = image();
    let (layout, backing) = prepare(&image, 0x20_0000, 0x4000_0000);
    let slots = ModuleSlots::<1>::new();
    let module = unsafe { slots.publish(layout, backing, []) }.ok().unwrap();
    let start = Barrier::new(2);
    {
        let raced = thread::scope(|scope| {
            let entrant = scope.spawn(|| {
                start.wait();
                module.acquire(GEOMETRY, 1)
            });
            start.wait();
            module.shut();
            entrant.join().unwrap()
        });
        assert!(matches!(module.acquire(GEOMETRY, 1), Err(UseError::Closed)));
        match raced {
            Ok(held) => {
                assert_eq!(held.id(), module.id());
                assert!(matches!(module.retire(), Err(UseError::Busy)));
                drop(held);
            }
            Err(error) => assert_eq!(error, UseError::Closed),
        }
    }

    unsafe { module.retire().unwrap().complete() };
    drop(module.reclaim().ok().unwrap());
}

#[test]
fn a_consumer_holds_the_exact_provider_until_its_custody_is_reclaimed() {
    let image = image();
    let (first_layout, first_backing) = prepare(&image, 0x20_0000, 0x4000_0000);
    let (second_layout, second_backing) = prepare(&image, 0x21_0000, 0x4001_0000);
    let (consumer_layout, consumer_backing) = prepare(&image, 0x22_0000, 0x4002_0000);
    let slots = ModuleSlots::<2>::new();
    let independent = ModuleSlots::<1>::new();
    // All three use disjoint Host simulation buffers. The two providers
    // come from the same immutable ELF and use the first slot in each table.
    let first = unsafe { slots.publish(first_layout, first_backing, []) }
        .ok()
        .unwrap();
    let second = unsafe { independent.publish(second_layout, second_backing, []) }
        .ok()
        .unwrap();
    assert_ne!(first.id(), second.id());
    let held = first.acquire(GEOMETRY, 1).unwrap();
    let consumer = unsafe { slots.publish(consumer_layout, consumer_backing, [held]) }
        .ok()
        .unwrap();
    assert_ne!(consumer.id(), first.id());

    first.shut();
    second.shut();
    assert!(matches!(first.retire(), Err(UseError::Busy)));
    // The dependency on the first load cannot affect the same image's
    // independently loaded second instance.
    unsafe { second.retire().unwrap().complete() };
    drop(second.reclaim().ok().unwrap());

    consumer.shut();
    let pending = consumer.retire().unwrap();
    assert!(matches!(first.retire(), Err(UseError::Busy)));
    unsafe { pending.complete() };
    assert!(matches!(first.retire(), Err(UseError::Busy)));
    drop(consumer.reclaim().ok().unwrap());
    unsafe { first.retire().unwrap().complete() };
    drop(first.reclaim().ok().unwrap());
}

#[test]
fn capacity_failure_keeps_the_layout_material_and_dependency() {
    let image = image();
    let (layout, backing) = prepare(&image, 0x20_0000, 0x4000_0000);
    let (next_layout, next_backing) = prepare(&image, 0x21_0000, 0x4001_0000);
    let next_physical = next_layout.physical();
    let next_virtual = next_layout.virtual_range();
    let next_entry = next_layout.entry(GEOMETRY);
    let next_storage = next_backing.as_ptr();
    let slots = ModuleSlots::<1>::new();
    let module = unsafe { slots.publish(layout, backing, []) }.ok().unwrap();
    let held = module.acquire(GEOMETRY, 1).unwrap();
    let (next_layout, next_backing, [held]) =
        unsafe { slots.publish(next_layout, next_backing, [held]) }
            .err()
            .unwrap();
    assert_eq!(next_layout.physical(), next_physical);
    assert_eq!(next_layout.virtual_range(), next_virtual);
    assert_eq!(next_layout.entry(GEOMETRY), next_entry);
    assert_eq!(next_backing.as_ptr(), next_storage);
    assert_eq!(held.id(), module.id());

    module.shut();
    assert!(matches!(module.retire(), Err(UseError::Busy)));
    drop(held);
    unsafe { module.retire().unwrap().complete() };
    drop(module.reclaim().ok().unwrap());
    assert!(
        unsafe { slots.publish(next_layout, next_backing, []) }.is_err(),
        "a boot-only identity slot cannot be reused after retirement"
    );
}
