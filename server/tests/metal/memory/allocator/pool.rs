use super::source::bootstrap;
use core::{
    cell::Cell,
    mem::{MaybeUninit, size_of},
    ptr,
};
use std::vec::Vec;

use crate::{
    entry::boot::tests::range,
    metal::{
        memory::allocator::{
            Allocation, Allowance, Config, Initial, MappedStorage,
            pool::{
                Capacity, Error, Geometry, GroupSlot, Pool, REFERENCE_LIMIT, ReleaseCursor,
                ReleaseProgress, Retirement, Stable,
            },
        },
        memory::mmu::internal::access::MappedRange,
        sync::{TurnLock, TurnMode::Fast, TurnWorkspace},
    },
};

use limen_treaty::model::ByteLength;

const PAGE: usize = 4096;
const CONFIG: Config = Config {
    granule: ByteLength::new(PAGE as u64),
    maximum: ByteLength::new(16 * PAGE as u64),
    regions: 3,
    grains: 16,
    roots: 8,
    allocations: 4,
    returns: 6,
    allowances: 2,
    edits: 128,
};

#[repr(align(4096))]
struct Page([MaybeUninit<u8>; PAGE]);

fn retain<R>(mut attempt: impl FnMut() -> Result<R, Error>) -> R {
    for _ in 0..16 {
        match attempt() {
            Ok(reference) => return reference,
            Err(Error::Busy) => {}
            Err(error) => panic!("unexpected retain error: {error:?}"),
        }
    }
    panic!("retain stayed Busy for 16 attempts");
}

// The Host fixture models one exact exclusive mapping of this Allocation.
// Pair it using the same production constructor as the target consumers.
fn mapped<'a>(
    material: &mut Option<Allocation>,
    bytes: &'a mut [MaybeUninit<u8>],
) -> Option<MappedStorage<'a>> {
    let allocation = material.take().unwrap();
    let mapping = unsafe { MappedRange::new(allocation.physical(), bytes) }.unwrap();
    Some(
        unsafe { MappedStorage::prepare(allocation, mapping) }
            .ok()
            .unwrap(),
    )
}

fn supplies() -> [Option<Allocation>; 2] {
    supply().0
}

fn supply() -> ([Option<Allocation>; 2], Allowance) {
    let mut blocks = [None, None];
    let mut allowance = None;
    bootstrap(|regions, mapping| {
        let base = regions.entries().next().unwrap().physical.start().get();
        let [first, second] = &mut blocks;
        let page = |index| range(base + index * PAGE as u64, base + (index + 1) * PAGE as u64);
        let mut initial = [Initial::new(page(0), first), Initial::new(page(1), second)];
        unsafe {
            regions.allocator(
                mapping,
                CONFIG,
                2 * PAGE as u64,
                &mut allowance,
                &mut initial,
            )
        }
        .unwrap()
        .unwrap();
    });
    (blocks, allowance.unwrap())
}

#[test]
fn installation_rejection_keeps_the_exact_allocation_and_mapping() {
    let [mut material, mut second] = supplies();
    let first_id = material.as_ref().unwrap().id();
    let second_id = second.as_ref().unwrap().id();
    let physical = material.as_ref().unwrap().physical();
    let second_physical = second.as_ref().unwrap().physical();
    let mut directory = [GroupSlot::<u64>::EMPTY];
    let mut bytes = Page([MaybeUninit::uninit(); PAGE]);
    let mut extra = Page([MaybeUninit::uninit(); PAGE]);
    let bytes_address = bytes.0.as_mut_ptr().addr();
    let pool = Pool::new(&mut directory).unwrap();
    let mut receipt1 = None;
    assert_eq!(
        unsafe { pool.install(&mut None, &mut receipt1) },
        Err(Error::Material)
    );

    let short = range(physical.start().get(), physical.end().get() - 1);
    let mapping = unsafe { MappedRange::new(short, &mut bytes.0[..PAGE - 1]) }.unwrap();
    let (allocation, mut returned, reason) =
        unsafe { MappedStorage::prepare(material.take().unwrap(), mapping) }
            .err()
            .unwrap();
    assert_eq!(reason, crate::metal::memory::allocator::Error::Range);
    assert_eq!(allocation.id(), first_id);
    assert_eq!(returned.physical(), short);
    assert_eq!(returned.bytes().as_mut_ptr().addr(), bytes_address);

    // Even an equally long, correctly aligned mapping of the other actual
    // Allocation cannot be paired with the first. Both inputs survive.
    let mapping = unsafe { MappedRange::new(second_physical, &mut bytes.0) }.unwrap();
    let (allocation, mut returned, reason) = unsafe { MappedStorage::prepare(allocation, mapping) }
        .err()
        .unwrap();
    assert_eq!(reason, crate::metal::memory::allocator::Error::Range);
    assert_eq!(allocation.id(), first_id);
    assert_eq!(returned.physical(), second_physical);
    assert_eq!(returned.bytes().as_mut_ptr().addr(), bytes_address);
    assert_eq!(pool.capacity().unwrap().groups, 0);

    material = Some(allocation);
    let mut paired = mapped(&mut material, &mut extra.0);
    let mut receipt2 = None;
    let first = unsafe { pool.install(&mut paired, &mut receipt2) }.unwrap();
    let supply = pool.supply(first).unwrap();
    assert_eq!(supply.allocation, first_id);
    assert_eq!(supply.geometry.slots, PAGE / supply.geometry.stride);
    assert!(supply.geometry.stride > size_of::<u64>());
    assert!(material.is_none() && paired.is_none());

    let mut paired = Some(
        unsafe { MappedStorage::prepare(second.take().unwrap(), returned) }
            .ok()
            .unwrap(),
    );
    let mut receipt3 = None;
    assert_eq!(
        unsafe { pool.install(&mut paired, &mut receipt3) },
        Err(Error::Directory)
    );
    let (allocation, mut mapping) = paired.take().unwrap().into_parts();
    assert_eq!(allocation.id(), second_id);
    assert_eq!(mapping.physical(), second_physical);
    assert_eq!(mapping.bytes().as_mut_ptr().addr(), bytes_address);
    assert_eq!(pool.capacity().unwrap().groups, 1);
}

#[test]
fn prepaid_position_rejects_material_from_another_construction_at_the_same_address() {
    let [mut material, _] = supplies();
    let physical = material.as_ref().unwrap().physical();
    let original = material.as_ref().unwrap().id();
    let mut directory = [GroupSlot::<u64>::EMPTY];
    let mut bytes = Page([MaybeUninit::uninit(); PAGE]);
    let pool = Pool::bounded(&mut directory, PAGE).unwrap();
    let mut growth = Some(std::boxed::Box::leak(std::boxed::Box::new(
        crate::metal::memory::allocator::pool::Growth::new(physical),
    )));
    let (group, receiver) = pool.prepare(&mut growth).unwrap();
    let mut paired = mapped(&mut material, &mut bytes.0);
    let mut receipt = None;
    assert_eq!(
        unsafe { pool.install_prepared(group, &mut paired, &mut receipt) },
        Err(Error::Material)
    );
    assert_eq!(paired.as_ref().unwrap().id(), original);
    assert!(receipt.is_none());
    assert_eq!(pool.capacity().unwrap().slots, 0);
    receiver.shut().unwrap();
    pool.discard_empty(group).unwrap();
    let published = unsafe { pool.install(&mut paired, &mut receipt) }.unwrap();
    assert_ne!(published, group);
    assert_eq!(pool.supply(published).unwrap().allocation, original);
}

#[test]
fn the_layout_counts_alignment_state_and_zero_sized_values() {
    #[repr(align(128))]
    struct Overaligned;

    let [mut material, _] = supplies();
    let mut directory = [GroupSlot::<Overaligned>::EMPTY];
    let mut bytes = Page([MaybeUninit::uninit(); PAGE]);
    let pool = Pool::new(&mut directory).unwrap();
    let mut paired = mapped(&mut material, &mut bytes.0);
    let id = paired.as_ref().unwrap().id();
    let mut receipt4 = None;
    assert_eq!(
        unsafe { pool.install(&mut paired, &mut receipt4) },
        Err(Error::Layout)
    );
    assert_eq!(paired.as_ref().unwrap().id(), id);
    assert!(material.is_none());

    use crate::metal::memory::allocator::pool::{VALUE_ALIGNMENT, VALUE_BYTES};
    #[repr(align(64))]
    struct Boundary([u8; VALUE_BYTES]);
    assert_eq!(size_of::<Boundary>(), VALUE_BYTES);
    assert_eq!(core::mem::align_of::<Boundary>(), VALUE_ALIGNMENT);
    assert_eq!(
        Geometry::of::<Boundary>(PAGE).unwrap().alignment,
        VALUE_ALIGNMENT
    );
    assert_eq!(
        Geometry::of::<[u8; VALUE_BYTES + 1]>(PAGE),
        Err(Error::Layout)
    );
    assert_eq!(
        Geometry::of::<[u8; 16 * PAGE]>(32 * PAGE),
        Err(Error::Layout)
    );
    assert_eq!(Boundary([0; VALUE_BYTES]).0[0], 0);

    let geometry = Geometry::of::<()>(PAGE).unwrap();
    assert!(geometry.stride > 0);
    assert_eq!(geometry.slots * geometry.stride + geometry.tail, PAGE);
    assert_eq!(Geometry::of::<()>(geometry.stride - 1), Err(Error::Layout));
}

#[test]
fn partial_groups_take_priority_and_destroyed_slots_wait_for_settlement() {
    struct Record<'a> {
        drops: &'a Cell<usize>,
        value: usize,
    }
    impl Drop for Record<'_> {
        fn drop(&mut self) {
            // A fresh Turn must be legal while the object's destructor runs.
            let own = TurnLock::new(());
            own.acquire(|_| ()).unwrap();
            self.drops.set(self.drops.get() + 1);
        }
    }

    let drops = Cell::new(0);
    let [mut first, mut second] = supplies();
    let mut directory = [const { GroupSlot::<Record<'_>>::EMPTY }; 2];
    let mut a = Page([MaybeUninit::uninit(); PAGE]);
    let mut b = Page([MaybeUninit::uninit(); PAGE]);
    let pool = Pool::new(&mut directory).unwrap();
    let mut receipt5 = None;
    let first = unsafe { pool.install(&mut mapped(&mut first, a.0.as_mut_slice()), &mut receipt5) }
        .unwrap();
    let mut receipt6 = None;
    let second =
        unsafe { pool.install(&mut mapped(&mut second, b.0.as_mut_slice()), &mut receipt6) }
            .unwrap();
    let count = pool.supply(first).unwrap().geometry.slots;
    assert!(count >= 2);

    let mut reservation = None;
    let mut owner = None;
    pool.reserve(&mut reservation).unwrap();
    assert_eq!(pool.reserve(&mut reservation), Err(Error::Receiver));
    let prepared = reservation.take().unwrap().initialize(Record {
        drops: &drops,
        value: 1,
    });
    prepared.publish(&mut owner).ok().unwrap();
    let object = owner.take().unwrap();
    assert_eq!(object.get().value, 1);
    let address = ptr::from_ref(object.get()).addr();

    let mut claims = Vec::new();
    for _ in 1..count {
        pool.reserve(&mut reservation).unwrap();
        claims.push(reservation.take().unwrap());
    }
    assert_eq!(pool.supply(second).unwrap().free, 0);
    assert_eq!(pool.supply(first).unwrap().free, count);
    for _ in 0..count {
        pool.reserve(&mut reservation).unwrap();
        claims.push(reservation.take().unwrap());
    }
    assert_eq!(pool.reserve(&mut reservation), Err(Error::Full));

    let vacant = object.destroy().ok().unwrap();
    assert_eq!(drops.get(), 1);
    assert_eq!(pool.reserve(&mut reservation), Err(Error::Full));
    vacant.release().ok().unwrap();
    pool.reserve(&mut reservation).unwrap();
    let cancelled = reservation.take().unwrap().initialize(Record {
        drops: &drops,
        value: 2,
    });
    cancelled.publish(&mut owner).ok().unwrap();
    assert_eq!(ptr::from_ref(owner.as_ref().unwrap().get()).addr(), address);
    let object = owner.take().unwrap();

    let mut reservation = claims.pop().unwrap();
    reservation = reservation
        .initialize(Record {
            drops: &drops,
            value: 3,
        })
        .destroy()
        .ok()
        .unwrap();
    reservation.cancel().ok().unwrap();
    object.destroy().ok().unwrap().release().ok().unwrap();
    for claim in claims {
        claim.cancel().ok().unwrap();
    }
    assert_eq!(drops.get(), 3);
    assert_eq!(
        pool.capacity().unwrap(),
        Capacity {
            groups: 2,
            bytes: 2 * PAGE,
            slots: 2 * count,
            free: 2 * count
        }
    );
}

#[test]
fn simultaneous_claims_do_not_share_a_slot() {
    use std::{sync::Barrier, thread};

    let [mut material, _] = supplies();
    let mut directory = [GroupSlot::<u64>::EMPTY];
    let mut bytes = Page([MaybeUninit::uninit(); PAGE]);
    let pool = Pool::new(&mut directory).unwrap();
    let mut receipt7 = None;
    unsafe {
        pool.install(
            &mut mapped(&mut material, bytes.0.as_mut_slice()),
            &mut receipt7,
        )
    }
    .unwrap();
    let ready = Barrier::new(2);
    let pool = &pool;
    let addresses = thread::scope(|scope| {
        let claim = |value| {
            let ready = &ready;
            scope.spawn(move || {
                let mut reserved = None;
                pool.reserve(&mut reserved).unwrap();
                let mut held = None;
                reserved
                    .take()
                    .unwrap()
                    .initialize(value)
                    .publish(&mut held)
                    .ok()
                    .unwrap();
                let held = held.unwrap();
                let address = ptr::from_ref(held.get()).addr();
                ready.wait();
                assert_eq!(*held.get(), value);
                held.destroy().ok().unwrap().release().ok().unwrap();
                address
            })
        };
        let first = claim(11);
        let second = claim(23);
        [first.join().unwrap(), second.join().unwrap()]
    });
    assert_ne!(addresses[0], addresses[1]);
    let capacity = pool.capacity().unwrap();
    assert_eq!(capacity.free, capacity.slots);
}

#[test]
fn reserve_and_publish_commit_into_the_held_consumer_record() {
    use crate::metal::memory::allocator::Error as SourceError;

    let [mut material, _] = supplies();
    let mut directory = [GroupSlot::<u64>::EMPTY];
    let mut bytes = Page([MaybeUninit::uninit(); PAGE]);
    let pool = Pool::new(&mut directory).unwrap();
    let mut receipt8 = None;
    unsafe {
        pool.install(
            &mut mapped(&mut material, bytes.0.as_mut_slice()),
            &mut receipt8,
        )
    }
    .unwrap();
    let free = pool.capacity().unwrap().free;
    let record = TurnLock::new((None, None));
    let mut work = TurnWorkspace::<2>::new();

    let mut plan = work.plan();
    plan.insert(&record).unwrap();
    plan.acquire(Fast, |held| {
        assert!(held.access_pair(&record, &record).is_none());
        assert_eq!(
            pool.reserve_held(held, &record, |record| &mut record.0),
            Err(Error::Source(SourceError::Protection))
        );
        assert!(held.access(&record).unwrap().0.is_none());
    })
    .unwrap();
    assert_eq!(pool.capacity().unwrap().free, free);

    let mut plan = work.plan();
    plan.insert(&record).unwrap();
    pool.plan(&mut plan).unwrap();
    plan.acquire(Fast, |held| {
        pool.reserve_held(held, &record, |record| &mut record.0)
            .unwrap();
        assert_eq!(
            pool.reserve_held(held, &record, |record| &mut record.0),
            Err(Error::Receiver)
        );
    })
    .unwrap();
    assert_eq!(pool.capacity().unwrap().free, free - 1);

    let mut plan = work.plan();
    plan.insert(&record).unwrap();
    let reservation = plan
        .acquire(Fast, |held| held.access(&record).unwrap().0.take().unwrap())
        .unwrap();
    let prepared = reservation.initialize(17);

    let mut plan = work.plan();
    pool.plan(&mut plan).unwrap();
    let (prepared, error) = plan
        .acquire(Fast, |held| {
            prepared.publish_held(held, &record, |record| &mut record.1)
        })
        .unwrap()
        .err()
        .unwrap();
    assert_eq!(error, Error::Source(SourceError::Protection));

    let mut plan = work.plan();
    pool.plan(&mut plan).unwrap();
    plan.insert(&record).unwrap();
    plan.acquire(Fast, |held| {
        assert!(held.access(&record).unwrap().1.is_none());
        prepared
            .publish_held(held, &record, |record| &mut record.1)
            .ok()
            .unwrap();
        assert_eq!(*held.access(&record).unwrap().1.as_ref().unwrap().get(), 17);
    })
    .unwrap();

    let mut plan = work.plan();
    plan.insert(&record).unwrap();
    let object = plan
        .acquire(Fast, |held| held.access(&record).unwrap().1.take().unwrap())
        .unwrap();
    object.destroy().ok().unwrap().release().ok().unwrap();
    assert_eq!(pool.capacity().unwrap().free, free);
}

#[test]
fn all_unique_handles_transfer_a_send_value_between_workers() {
    use std::{sync::mpsc::sync_channel, thread};

    let [mut material, _] = supplies();
    let mut directory = [GroupSlot::<Cell<u64>>::EMPTY];
    let mut bytes = Page([MaybeUninit::uninit(); PAGE]);
    let pool = Pool::new(&mut directory).unwrap();
    let mut receipt9 = None;
    unsafe {
        pool.install(
            &mut mapped(&mut material, bytes.0.as_mut_slice()),
            &mut receipt9,
        )
    }
    .unwrap();
    let pool = &pool;

    let (reserve, reserved) = sync_channel(1);
    let (prepare, prepared) = sync_channel(1);
    let (occupy, occupied) = sync_channel(1);
    let (settle, vacant) = sync_channel(1);
    thread::scope(|scope| {
        let first = scope.spawn(move || {
            let mut receipt = None;
            pool.reserve(&mut receipt).unwrap();
            reserve.send(receipt.unwrap()).ok().unwrap();
            let value: crate::metal::memory::allocator::pool::Prepared<'_, '_, Cell<u64>> =
                prepared.recv().unwrap();
            let mut receiver = None;
            value.publish(&mut receiver).ok().unwrap();
            occupy.send(receiver.unwrap()).ok().unwrap();
            let destroyed: crate::metal::memory::allocator::pool::DestructionReceipt<
                '_,
                '_,
                Cell<u64>,
            > = vacant.recv().unwrap();
            destroyed.release().ok().unwrap();
        });
        let second = scope.spawn(move || {
            let token: crate::metal::memory::allocator::pool::Reservation<'_, '_, Cell<u64>> =
                reserved.recv().unwrap();
            prepare.send(token.initialize(Cell::new(71))).ok().unwrap();
            let object: crate::metal::memory::allocator::pool::Occupied<'_, '_, Cell<u64>> =
                occupied.recv().unwrap();
            assert_eq!(object.get().get(), 71);
            object.get().set(73);
            assert_eq!(object.get().get(), 73);
            settle.send(object.destroy().ok().unwrap()).ok().unwrap();
        });
        first.join().unwrap();
        second.join().unwrap();
    });
    let capacity = pool.capacity().unwrap();
    assert_eq!(capacity.free, capacity.slots);
}

#[test]
fn stable_references_hold_the_detached_slot_until_an_exclusive_reclaim_settles() {
    use crate::metal::memory::allocator::pool::Reclamation;
    use core::sync::atomic::{AtomicUsize, Ordering::Relaxed};
    use std::{sync::mpsc::sync_channel, thread};

    struct Record<'a>(&'a AtomicUsize);
    impl Drop for Record<'_> {
        fn drop(&mut self) {
            let lock = TurnLock::new(());
            lock.acquire(|_| self.0.fetch_add(1, Relaxed)).unwrap();
        }
    }

    let [mut material, _] = supplies();
    let mut directory = [GroupSlot::<Stable<Record<'_>>>::EMPTY];
    let mut page = Page([MaybeUninit::uninit(); PAGE]);
    let drops = AtomicUsize::new(0);
    let pool = Pool::new(&mut directory).unwrap();
    let mut receipt10 = None;
    let group =
        unsafe { pool.install(&mut mapped(&mut material, &mut page.0), &mut receipt10) }.unwrap();
    let slots = pool.capacity().unwrap().slots;
    let mut reserved = None;
    let mut occupied = None;
    pool.reserve(&mut reserved).unwrap();
    reserved
        .take()
        .unwrap()
        .initialize(Stable::new(Record(&drops)))
        .publish(&mut occupied)
        .ok()
        .unwrap();

    let original = TurnLock::new(occupied.take().map(|object| object.stabilize()));
    let reference = original
        .acquire(|original| {
            let owner = original.as_ref().unwrap();
            retain(|| owner.reference())
        })
        .unwrap();
    let address = ptr::from_ref(reference.get()).addr();
    let mut copies = Vec::new();
    for _ in 0..REFERENCE_LIMIT - 2 {
        copies.push(retain(|| reference.try_retain()));
    }
    assert_eq!(reference.try_retain().err(), Some(Error::References));
    copies.clear();
    let detached = original
        .acquire(|original| original.take().unwrap().detach())
        .unwrap();

    let recovery = TurnLock::new(None::<Reclamation<'_, '_, Record<'_>>>);
    let (release, wait) = sync_channel(0);
    thread::scope(|scope| {
        let old = scope.spawn(move || {
            // A holder can still retain while the base is detached.
            let old = retain(|| reference.try_retain());
            assert_eq!(ptr::from_ref(old.get()).addr(), address);
            wait.recv().unwrap();
        });
        let mut work = TurnWorkspace::<2>::new();
        let mut plan = work.plan();
        pool.plan(&mut plan).unwrap();
        plan.insert(&recovery).unwrap();
        let result = plan
            .acquire(Fast, |held| {
                detached.claim_held(held, &recovery, |slot| slot)
            })
            .unwrap();
        release.send(()).unwrap();
        let (detached, error) = result.err().unwrap();
        assert_eq!(error, Error::References);
        old.join().unwrap();

        assert_eq!(drops.load(Relaxed), 0);
        assert_eq!(pool.capacity().unwrap().free, slots - 1);
        assert_eq!(pool.shut(group), Err(Error::Busy));
        let mut plan = work.plan();
        pool.plan(&mut plan).unwrap();
        plan.insert(&recovery).unwrap();
        plan.acquire(Fast, |held| {
            detached.claim_held(held, &recovery, |slot| slot)
        })
        .unwrap()
        .ok()
        .unwrap();
        let mut plan = work.plan();
        plan.insert(&recovery).unwrap();
        let reclaimed = plan
            .acquire(Fast, |held| held.access(&recovery).unwrap().take().unwrap())
            .unwrap();
        scope.spawn(move || {
            let _receipt = reclaimed.destroy().ok().unwrap();
        });
    });

    assert_eq!(drops.load(Relaxed), 1);
    assert_eq!(pool.shut(group), Err(Error::Busy));
    assert_eq!(pool.capacity().unwrap().free, slots - 1);
    // The destructor's worker exited without passing back a slot token.
    let mut cursor = ReleaseCursor::default();
    assert_eq!(
        pool.release_destroyed(&mut cursor, 0),
        Ok(ReleaseProgress::More)
    );
    assert_eq!(
        pool.release_destroyed(&mut cursor, 1),
        Ok(ReleaseProgress::Released)
    );
    pool.reserve(&mut reserved).unwrap();
    reserved
        .take()
        .unwrap()
        .initialize(Stable::new(Record(&drops)))
        .publish(&mut occupied)
        .ok()
        .unwrap();
    let owner = occupied.take().unwrap().stabilize();
    let reference = retain(|| owner.reference());
    assert_eq!(ptr::from_ref(reference.get()).addr(), address);
    drop(reference);
    let mut reclaim = None;
    owner.detach().claim(&mut reclaim).ok().unwrap();
    reclaim
        .unwrap()
        .destroy()
        .ok()
        .unwrap()
        .release()
        .ok()
        .unwrap();
    assert_eq!(drops.load(Relaxed), 2);
    assert_eq!(pool.capacity().unwrap().free, slots);
}

#[test]
fn recovery_releases_an_ordinary_object_after_its_destroyer_exits() {
    use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

    struct Record<'a>(&'a AtomicUsize);

    impl Drop for Record<'_> {
        fn drop(&mut self) {
            self.0.fetch_add(1, Relaxed);
        }
    }

    let [mut material, _] = supplies();
    let mut directory = [GroupSlot::<Record<'_>>::EMPTY];
    let mut page = Page([MaybeUninit::uninit(); PAGE]);
    let drops = AtomicUsize::new(0);
    let pool = Pool::new(&mut directory).unwrap();
    let mut receipt11 = None;
    let group =
        unsafe { pool.install(&mut mapped(&mut material, &mut page.0), &mut receipt11) }.unwrap();
    let slots = pool.capacity().unwrap().slots;
    let mut reserved = None;
    let mut occupied = None;
    pool.reserve(&mut reserved).unwrap();
    reserved
        .unwrap()
        .initialize(Record(&drops))
        .publish(&mut occupied)
        .ok()
        .unwrap();
    std::thread::scope(|scope| {
        scope
            .spawn(move || {
                let _receipt = occupied.unwrap().destroy().ok().unwrap();
            })
            .join()
            .unwrap();
    });

    assert_eq!(pool.shut(group), Err(Error::Busy));
    assert_eq!(pool.capacity().unwrap().free, slots - 1);
    assert_eq!(
        pool.release_destroyed(&mut ReleaseCursor::default(), 1),
        Ok(ReleaseProgress::Released)
    );
    assert_eq!(pool.capacity().unwrap().free, slots);
    assert_eq!(drops.load(Relaxed), 1);
}

#[test]
fn a_direct_completion_beyond_the_scan_budget_cannot_release_a_later_object() {
    let [mut material, _] = supplies();
    let mut directory = [GroupSlot::<Stable<u64>>::EMPTY];
    let mut page = Page([MaybeUninit::uninit(); PAGE]);
    let pool = Pool::new(&mut directory).unwrap();
    let mut receipt12 = None;
    let group =
        unsafe { pool.install(&mut mapped(&mut material, &mut page.0), &mut receipt12) }.unwrap();
    let slots = pool.capacity().unwrap().slots;
    assert!(slots > 64);
    let mut prefix = [const { None }; 64];
    for place in &mut prefix {
        pool.reserve(place).unwrap();
    }
    let destroy = || {
        let mut reserved = None;
        let mut occupied = None;
        let mut reclamation = None;
        pool.reserve(&mut reserved).unwrap();
        reserved
            .unwrap()
            .initialize(Stable::new(37))
            .publish(&mut occupied)
            .ok()
            .unwrap();
        let owner = occupied.unwrap().stabilize();
        let reference = retain(|| owner.reference());
        let address = ptr::from_ref(reference.get()).addr();
        drop(reference);
        owner.detach().claim(&mut reclamation).ok().unwrap();
        (address, reclamation.unwrap().destroy().ok().unwrap())
    };
    let (address, direct) = destroy();
    assert_eq!(
        pool.release_destroyed(&mut ReleaseCursor::default(), 64),
        Ok(ReleaseProgress::More)
    );
    assert_eq!(pool.capacity().unwrap().free, slots - 65);
    direct.release().ok().unwrap();
    assert_eq!(pool.capacity().unwrap().free, slots - 64);

    let (original, old) = destroy();
    assert_eq!(original, address);
    let mut cursor = ReleaseCursor::default();
    assert_eq!(
        pool.release_destroyed(&mut cursor, 64),
        Ok(ReleaseProgress::More)
    );
    assert_eq!(
        pool.release_destroyed(&mut cursor, 1),
        Ok(ReleaseProgress::Released)
    );
    let (reused, current) = destroy();
    assert_eq!(reused, address);
    let (old, error) = old.release().err().unwrap();
    assert_eq!(error, Error::Stale);
    assert_eq!(pool.capacity().unwrap().free, slots - 65);
    current.release().ok().unwrap();
    for place in prefix {
        place.unwrap().cancel().ok().unwrap();
    }
    pool.shut(group).unwrap();
    assert_eq!(old.release().err().unwrap().1, Error::Stale);
}

#[test]
fn a_destructor_with_unknown_outcome_does_not_make_the_slot_reusable() {
    struct Fault;

    impl Drop for Fault {
        fn drop(&mut self) {
            panic!("destructor fault");
        }
    }

    let [mut material, _] = supplies();
    let mut directory = [GroupSlot::<Fault>::EMPTY];
    let mut page = Page([MaybeUninit::uninit(); PAGE]);
    let pool = Pool::new(&mut directory).unwrap();
    let mut receipt13 = None;
    let group =
        unsafe { pool.install(&mut mapped(&mut material, &mut page.0), &mut receipt13) }.unwrap();
    let slots = pool.capacity().unwrap().slots;
    let mut reserved = None;
    let mut occupied = None;
    pool.reserve(&mut reserved).unwrap();
    reserved
        .unwrap()
        .initialize(Fault)
        .publish(&mut occupied)
        .ok()
        .unwrap();
    assert!(std::thread::scope(|scope| {
        scope
            .spawn(move || {
                let _receipt = occupied.unwrap().destroy().ok().unwrap();
            })
            .join()
            .is_err()
    }));

    let mut cursor = ReleaseCursor::default();
    for _ in 0..slots {
        assert_eq!(
            pool.release_destroyed(&mut cursor, 1),
            Ok(ReleaseProgress::More)
        );
    }
    assert_eq!(
        pool.release_destroyed(&mut cursor, 2),
        Ok(ReleaseProgress::End)
    );
    assert_eq!(pool.capacity().unwrap().free, slots - 1);
    assert_eq!(pool.shut(group), Err(Error::Busy));
}

#[test]
fn a_shut_group_keeps_its_original_charge_and_receipt_through_refund() {
    use crate::metal::memory::allocator::{Error as SourceError, Pending};

    let ([mut a, mut b], allowance) = supply();
    let region = a.as_ref().unwrap().region();
    let source = allowance.source();
    let mut directory = [const { GroupSlot::<u64>::EMPTY }; 2];
    let mut left = Page([MaybeUninit::uninit(); PAGE]);
    let mut right = Page([MaybeUninit::uninit(); PAGE]);
    let pool = Pool::new(&mut directory).unwrap();
    let (original, b) = {
        let mut receipt14 = None;
        let a = unsafe { pool.install(&mut mapped(&mut a, &mut left.0), &mut receipt14) }.unwrap();
        let mut receipt15 = None;
        let b = unsafe { pool.install(&mut mapped(&mut b, &mut right.0), &mut receipt15) }.unwrap();
        let original = pool.supply(a).unwrap();
        let pool = &pool;
        std::thread::scope(|scope| {
            scope
                .spawn(move || {
                    let physical = pool.shut(a).unwrap();
                    assert_eq!(pool.shut(a), Ok(physical));
                    assert_eq!(pool.supply(a), Err(Error::Stale));
                    assert_eq!(pool.finish_return(a, Fast), Err(Error::Busy));
                })
                .join()
                .unwrap();
        });
        (original, b)
    };
    let mut reserved = None;
    pool.reserve(&mut reserved).unwrap();
    assert_eq!(pool.shut(b), Err(Error::Busy));
    assert_eq!(pool.capacity().unwrap().groups, 1);
    assert_eq!(pool.capacity().unwrap().bytes, 2 * PAGE);
    assert_eq!(
        allowance.balance().unwrap().occupied().get(),
        2 * PAGE as u64
    );
    assert_eq!(source.pending(region, &mut 0, 64), Ok(Pending::End));

    let physical = {
        let Retirement::Shutting(a, physical) = pool.retirement(&mut 0, 1).unwrap() else {
            panic!("expected the original shut group from the Pool root");
        };
        // The fixture is the sole mapping owner and no slot from A escaped.
        unsafe { pool.begin_return(a, Fast) }.unwrap();
        physical
    };
    assert_eq!(
        source.pending(region, &mut 0, 64),
        Ok(Pending::Found(original.retirement))
    );
    let done = source.finish_return(original.retirement, Fast).unwrap();
    assert_eq!(source.finish_return(original.retirement, Fast), Ok(done));
    assert_eq!(done.physical, physical);
    assert_eq!(done.allocation, original.allocation);
    assert_eq!(pool.capacity().unwrap().bytes, 2 * PAGE);
    assert_eq!(allowance.balance().unwrap().occupied().get(), PAGE as u64);
    let Retirement::Returning(a) = pool.retirement(&mut 0, 1).unwrap() else {
        panic!("expected the handed-off group from the Pool root");
    };
    let (left, right) = std::thread::scope(|scope| {
        let left = scope.spawn(|| pool.finish_return(a, crate::metal::sync::TurnMode::Slow));
        let right = scope.spawn(|| pool.finish_return(a, crate::metal::sync::TurnMode::Slow));
        (left.join().unwrap(), right.join().unwrap())
    });
    assert_eq!(left, Ok(done));
    assert_eq!(right, Ok(done));
    assert_eq!(pool.finish_return(a, Fast), Ok(done));
    assert_eq!(
        source.finish_return(original.retirement, Fast),
        Err(SourceError::Stale)
    );
    assert_eq!(pool.capacity().unwrap().bytes, PAGE);
    let mut cursor = 0;
    assert_eq!(pool.retirement(&mut cursor, 1), Ok(Retirement::More));
    assert_eq!(pool.retirement(&mut cursor, 1), Ok(Retirement::End));
    assert_eq!(cursor, 0);

    reserved.take().unwrap().cancel().ok().unwrap();
    pool.shut(b).unwrap();
    assert_eq!(pool.reserve(&mut reserved), Err(Error::Full));
    unsafe { pool.begin_return(b, Fast) }.unwrap();
    pool.finish_return(b, Fast).unwrap();
    assert_eq!(
        pool.capacity().unwrap(),
        Capacity {
            groups: 0,
            bytes: 0,
            slots: 0,
            free: 0
        }
    );
    assert_eq!(allowance.balance().unwrap().occupied().get(), 0);
}

#[test]
fn an_id_from_another_pool_cannot_close_a_same_index_group() {
    let [mut a, mut b] = supplies();
    let mut left = [GroupSlot::<u64>::EMPTY];
    let mut right = [GroupSlot::<u64>::EMPTY];
    let mut old = Page([MaybeUninit::uninit(); PAGE]);
    let mut new = Page([MaybeUninit::uninit(); PAGE]);
    let first = Pool::new(&mut left).unwrap();
    let second = Pool::new(&mut right).unwrap();
    let mut receipt16 = None;
    let original =
        unsafe { first.install(&mut mapped(&mut a, &mut old.0), &mut receipt16) }.unwrap();
    let mut receipt17 = None;
    unsafe { second.install(&mut mapped(&mut b, &mut new.0), &mut receipt17) }.unwrap();
    assert_eq!(second.shut(original), Err(Error::Stale));
    assert_eq!(second.capacity().unwrap().groups, 1);
}

#[test]
fn retired_group_history_is_independent_of_runtime_directory_reuse() {
    let ([mut first, mut second], allowance) = supply();
    let mut directory = [GroupSlot::<u64>::EMPTY];
    let mut first_page = Page([MaybeUninit::uninit(); PAGE]);
    let mut next_page = Page([MaybeUninit::uninit(); PAGE]);
    let pool = Pool::new(&mut directory).unwrap();
    let mut first_receipt = None;
    let mut next_receipt = None;
    let mut first = mapped(&mut first, &mut first_page.0);
    let original = unsafe { pool.install(&mut first, &mut first_receipt) }.unwrap();
    let first_receipt = first_receipt.unwrap();
    assert_eq!(first_receipt.id(), original);
    assert_eq!(first_receipt.acknowledge(&pool), Err(Error::Busy));
    let mut next = mapped(&mut second, &mut next_page.0);
    pool.shut(original).unwrap();
    unsafe { pool.begin_return(original, Fast) }.unwrap();
    let returned = pool.finish_return(original, Fast).unwrap();
    assert_eq!(pool.finish_return(original, Fast), Ok(returned));
    assert_eq!(
        unsafe { pool.install(&mut next, &mut next_receipt) },
        Err(Error::Directory)
    );
    assert!(next.is_some() && next_receipt.is_none());
    first_receipt.acknowledge(&pool).unwrap();
    assert_eq!(pool.finish_return(original, Fast), Err(Error::Stale));
    let successor = unsafe { pool.install(&mut next, &mut next_receipt) }.unwrap();
    assert_ne!(original, successor);
    assert_eq!(first_receipt.acknowledge(&pool), Err(Error::Stale));
    assert_eq!(pool.shut(original), Err(Error::Stale));
    assert_eq!(pool.capacity().unwrap().groups, 1);
    let mut slot = None;
    pool.reserve(&mut slot).unwrap();
    assert_eq!(pool.shut(successor), Err(Error::Busy));
    slot.unwrap().cancel().ok().unwrap();
    pool.shut(successor).unwrap();
    unsafe { pool.begin_return(successor, Fast) }.unwrap();
    pool.finish_return(successor, Fast).unwrap();
    next_receipt.unwrap().acknowledge(&pool).unwrap();
    assert_eq!(allowance.balance().unwrap().occupied().get(), 0);
}
