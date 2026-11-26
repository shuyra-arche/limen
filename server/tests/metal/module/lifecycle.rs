use super::{IMAGES, PAGE, ROOT, SV39_ROOT};
use kernel::metal::{
    address::PhysicalAddress,
    mmu::internal::access::MappedRange,
    mmu::internal::{
        GEOMETRY,
        riscv::{Mode, root},
    },
    module::{Contract, Layout, Machine, ModuleImage, ModuleSlots, UseError},
};
use std::{mem::MaybeUninit, sync::Barrier, thread};

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
    let first = module.acquire(GEOMETRY).unwrap();
    let second = module.acquire(GEOMETRY).unwrap();
    assert_eq!(first.id(), module.id());
    assert_eq!(first.entry(), entry);
    assert!(matches!(
        module.acquire(Contract::new(b"shuyra,mmu-geometry", 1, 2)),
        Err(UseError::Interface)
    ));
    assert!(matches!(
        module.acquire(Contract::new(b"shuyra,mmu-geometry", 2, 1)),
        Err(UseError::Interface)
    ));

    module.shut();
    module.shut();
    assert!(matches!(module.acquire(GEOMETRY), Err(UseError::Shut)));
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
    assert!(matches!(module.acquire(GEOMETRY), Err(UseError::Shut)));
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
                module.acquire(GEOMETRY)
            });
            start.wait();
            module.shut();
            entrant.join().unwrap()
        });
        assert!(matches!(module.acquire(GEOMETRY), Err(UseError::Shut)));
        match raced {
            Ok(held) => {
                assert_eq!(held.id(), module.id());
                assert!(matches!(module.retire(), Err(UseError::Busy)));
                drop(held);
            }
            Err(error) => assert_eq!(error, UseError::Shut),
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
    let held = first.acquire(GEOMETRY).unwrap();
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
    let held = module.acquire(GEOMETRY).unwrap();
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

#[test]
fn root_publication_keeps_custody_until_its_exact_rv64_geometry_is_bound() {
    let (machine, bytes) = IMAGES[0];
    let geometry = ModuleImage::read(bytes, machine, PAGE).unwrap();
    let root = ModuleImage::read(SV39_ROOT, machine, PAGE).unwrap();
    let (geometry_layout, geometry_material) = prepare(&geometry, 0x20_0000, 0x4000_0000);
    let (other_layout, other_material) = prepare(&image(), 0x21_0000, 0x4001_0000);
    let (root_layout, root_material) = prepare(&root, 0x22_0000, 0x4002_0000);
    let (second_layout, second_material) = prepare(&root, 0x23_0000, 0x4003_0000);
    let root_physical = root_layout.physical();
    let root_entry = root_layout.entry(ROOT).unwrap();
    let root_storage = root_material.as_ptr();
    let slots = ModuleSlots::<3>::new();
    let geometry = unsafe { slots.publish(geometry_layout, geometry_material, []) }
        .ok()
        .unwrap();
    let other = unsafe { slots.publish(other_layout, other_material, []) }
        .ok()
        .unwrap();

    // These buffers only model custody. Their numerical entries are never
    // called on the Host.
    assert_eq!(
        root::check(Mode::Sv39, &root_layout, &[]),
        Err(UseError::Interface)
    );
    let wrong_machine = other.acquire(GEOMETRY).unwrap();
    assert_eq!(
        root::check(
            Mode::Sv39,
            &root_layout,
            std::slice::from_ref(&wrong_machine)
        ),
        Err(UseError::Interface)
    );
    assert_eq!(wrong_machine.id(), other.id());
    assert_eq!(wrong_machine.machine(), Machine::X86_64);
    drop(wrong_machine);

    let first = {
        let ambiguous = [
            geometry.acquire(GEOMETRY).unwrap(),
            geometry.acquire(GEOMETRY).unwrap(),
        ];
        assert_eq!(
            root::check(Mode::Sv39, &root_layout, &ambiguous),
            Err(UseError::Interface)
        );
        let [first, second] = ambiguous;
        assert_eq!(first.id(), geometry.id());
        assert_eq!(second.id(), geometry.id());
        assert_eq!(first.contract(), GEOMETRY);
        drop(second);
        first
    };
    assert_eq!(root_layout.physical(), root_physical);
    assert_eq!(root_layout.entry(ROOT), Some(root_entry));
    assert_eq!(root_material.as_ptr(), root_storage);

    let dependency = [first];
    root::check(Mode::Sv39, &root_layout, &dependency).unwrap();
    for mode in [Mode::Sv48, Mode::Sv57] {
        assert_eq!(
            root::check(mode, &root_layout, &dependency),
            Err(UseError::Interface)
        );
    }
    let root = unsafe { slots.publish(root_layout, root_material, dependency) }
        .ok()
        .unwrap();
    let wrong_contract = root.acquire(ROOT).unwrap();
    assert_eq!(
        root::check(
            Mode::Sv39,
            &second_layout,
            std::slice::from_ref(&wrong_contract)
        ),
        Err(UseError::Interface)
    );
    assert_eq!(wrong_contract.id(), root.id());
    assert_eq!(wrong_contract.contract(), ROOT);
    assert_eq!(wrong_contract.entry(), root_entry);
    drop(wrong_contract);
    drop(second_material);

    geometry.shut();
    assert!(matches!(geometry.retire(), Err(UseError::Busy)));
    root.shut();
    unsafe { root.retire().unwrap().complete() };
    let returned = root.reclaim().ok().unwrap();
    assert_eq!(returned.as_ptr(), root_storage);
    unsafe { geometry.retire().unwrap().complete() };
    drop(geometry.reclaim().ok().unwrap());
    other.shut();
    unsafe { other.retire().unwrap().complete() };
    drop(other.reclaim().ok().unwrap());
}
