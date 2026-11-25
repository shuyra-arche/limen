use core::{
    cell::Cell,
    mem::{MaybeUninit, size_of},
    ptr,
};

use crate::{
    Physical,
    allocator::{
        Allocation, Config, Initial,
        pool::{Capacity, Error, Geometry, GroupSlot, Pool},
    },
    source::bootstrap,
    sync::{Acquisition::Fast, TurnLock, Work},
};

const PAGE: usize = 4096;
const CONFIG: Config = Config {
    granule: PAGE as u64,
    maximum: 16 * PAGE as u64,
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

fn supplies() -> [Option<Allocation>; 2] {
    let mut blocks = [None, None];
    let mut allowance = None;
    bootstrap(|regions, mapping| {
        let base = regions.entries().next().unwrap().physical.start;
        let [first, second] = &mut blocks;
        let page = |index| Physical {
            start: base + index * PAGE as u64,
            end: base + (index + 1) * PAGE as u64,
        };
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
    blocks
}

#[test]
fn installation_rejection_keeps_the_exact_allocation_and_mapping() {
    let [mut material, mut second] = supplies();
    let first_id = material.as_ref().unwrap().id();
    let second_id = second.as_ref().unwrap().id();
    let mut directory = [GroupSlot::<u64>::EMPTY];
    let mut bytes = Page([MaybeUninit::uninit(); PAGE]);
    let mut extra = Page([MaybeUninit::uninit(); PAGE]);
    let mut pool = Pool::new(&mut directory).unwrap();
    assert_eq!(
        unsafe { pool.install(&mut material, &mut None) },
        Err(Error::Material)
    );
    let mut short = Some(&mut bytes.0[..PAGE - 1]);
    let short_addr = short.as_ref().unwrap().as_ptr();
    assert_eq!(
        unsafe { pool.install(&mut material, &mut short) },
        Err(Error::Material)
    );
    assert_eq!(material.as_ref().unwrap().id(), first_id);
    assert_eq!(short.as_ref().unwrap().as_ptr(), short_addr);
    assert_eq!(pool.capacity().unwrap().groups, 0);

    let mut mapped = Some(extra.0.as_mut_slice());
    let first = unsafe { pool.install(&mut material, &mut mapped) }.unwrap();
    let supply = pool.supply(first).unwrap();
    assert_eq!(supply.allocation, first_id);
    assert_eq!(supply.geometry.slots, PAGE / supply.geometry.stride);
    assert!(supply.geometry.stride > size_of::<u64>());
    assert!(material.is_none() && mapped.is_none());
    assert_eq!(
        unsafe { pool.install(&mut second, &mut short) },
        Err(Error::Directory)
    );
    assert_eq!(second.as_ref().unwrap().id(), second_id);
    assert_eq!(short.as_ref().unwrap().as_ptr(), short_addr);
    assert_eq!(pool.capacity().unwrap().groups, 1);
}

#[test]
fn the_layout_counts_alignment_state_and_zero_sized_values() {
    #[repr(align(128))]
    struct Overaligned;

    let [mut material, _] = supplies();
    let mut directory = [GroupSlot::<Overaligned>::EMPTY];
    let mut bytes = Page([MaybeUninit::uninit(); PAGE]);
    let mut pool = Pool::new(&mut directory).unwrap();
    let mut mapped = Some(bytes.0.as_mut_slice());
    assert_eq!(
        unsafe { pool.install(&mut material, &mut mapped) },
        Err(Error::Layout)
    );
    assert!(material.is_some() && mapped.is_some());

    use crate::allocator::pool::{VALUE_ALIGNMENT, VALUE_BYTES};
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
            let mut work = Work::<1>::new();
            let mut plan = work.plan();
            plan.insert(&own).unwrap();
            plan.execute(Fast, |_| {}).unwrap();
            self.drops.set(self.drops.get() + 1);
        }
    }

    let drops = Cell::new(0);
    let [mut first, mut second] = supplies();
    let mut directory = [const { GroupSlot::<Record<'_>>::EMPTY }; 2];
    let mut a = Page([MaybeUninit::uninit(); PAGE]);
    let mut b = Page([MaybeUninit::uninit(); PAGE]);
    let mut pool = Pool::new(&mut directory).unwrap();
    let first = unsafe { pool.install(&mut first, &mut Some(a.0.as_mut_slice())) }.unwrap();
    let second = unsafe { pool.install(&mut second, &mut Some(b.0.as_mut_slice())) }.unwrap();
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
    let mut pool = Pool::new(&mut directory).unwrap();
    unsafe { pool.install(&mut material, &mut Some(bytes.0.as_mut_slice())) }.unwrap();
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
    use crate::allocator::Error as SourceError;

    let [mut material, _] = supplies();
    let mut directory = [GroupSlot::<u64>::EMPTY];
    let mut bytes = Page([MaybeUninit::uninit(); PAGE]);
    let mut pool = Pool::new(&mut directory).unwrap();
    unsafe { pool.install(&mut material, &mut Some(bytes.0.as_mut_slice())) }.unwrap();
    let free = pool.capacity().unwrap().free;
    let record = TurnLock::new((None, None));
    let mut work = Work::<2>::new();

    let mut plan = work.plan();
    plan.insert(&record).unwrap();
    plan.execute(Fast, |held| {
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
    plan.execute(Fast, |held| {
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
        .execute(Fast, |held| held.access(&record).unwrap().0.take().unwrap())
        .unwrap();
    let prepared = reservation.initialize(17);

    let mut plan = work.plan();
    pool.plan(&mut plan).unwrap();
    let (prepared, error) = plan
        .execute(Fast, |held| {
            prepared.publish_held(held, &record, |record| &mut record.1)
        })
        .unwrap()
        .err()
        .unwrap();
    assert_eq!(error, Error::Source(SourceError::Protection));

    let mut plan = work.plan();
    pool.plan(&mut plan).unwrap();
    plan.insert(&record).unwrap();
    plan.execute(Fast, |held| {
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
        .execute(Fast, |held| held.access(&record).unwrap().1.take().unwrap())
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
    let mut pool = Pool::new(&mut directory).unwrap();
    unsafe { pool.install(&mut material, &mut Some(bytes.0.as_mut_slice())) }.unwrap();
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
            let value: crate::allocator::pool::Prepared<'_, '_, Cell<u64>> =
                prepared.recv().unwrap();
            let mut receiver = None;
            value.publish(&mut receiver).ok().unwrap();
            occupy.send(receiver.unwrap()).ok().unwrap();
            let destroyed: crate::allocator::pool::Vacant<'_, '_, Cell<u64>> =
                vacant.recv().unwrap();
            destroyed.release().ok().unwrap();
        });
        let second = scope.spawn(move || {
            let token: crate::allocator::pool::Reservation<'_, '_, Cell<u64>> =
                reserved.recv().unwrap();
            prepare.send(token.initialize(Cell::new(71))).ok().unwrap();
            let object: crate::allocator::pool::Occupied<'_, '_, Cell<u64>> =
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
