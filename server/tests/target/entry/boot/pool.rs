use core::{mem::MaybeUninit, ptr};

use kernel::{
    allocator::{
        Allocation, Allowance, Error as SourceError,
        pool::{Error, GroupSlot, Pool, Reference, ReleaseCursor, ReleaseProgress, Retirement, Stable, StableOwner},
    },
    entry::boot::{self, Boot},
    sync::{Acquisition::Fast, TurnLock, Work},
};

use super::pool_material::{GROUP, PAGE, install};

type Entry = Stable<TurnLock<usize>>;

static mut GROUPS: [GroupSlot<'static, Entry>; 1] = [GroupSlot::EMPTY];
static mut TYPED: MaybeUninit<Pool<'static, Entry>> = MaybeUninit::uninit();
static mut ALLOCATION: Option<Allocation> = None;
static mut ALLOWANCE: Option<Allowance> = None;

fn reference<'pool>(owner: &StableOwner<'pool, '_, TurnLock<usize>>) -> Reference<'pool, TurnLock<usize>> {
    for _ in 0..16 {
        match owner.reference() {
            Ok(reference) => return reference,
            Err(Error::Busy) => {}
            Err(error) => panic!("unexpected retain error: {error:?}"),
        }
    }
    panic!("retain stayed Busy for 16 attempts");
}

pub(super) fn check(boot: &Boot<'_>) {
    let allocation = unsafe { &mut *(&raw mut ALLOCATION) };
    let allowance = unsafe { &mut *(&raw mut ALLOWANCE) };
    let directory = unsafe { &mut *(&raw mut GROUPS) };
    let (pool, group, source) = unsafe { install(boot, directory, allocation, allowance) };
    let pool = unsafe { (&mut *(&raw mut TYPED)).write(pool) };

    let supply = pool.supply(group).unwrap();
    let geometry = supply.geometry;
    assert!(geometry.slots > 1 && geometry.alignment >= core::mem::align_of::<Entry>());
    assert_eq!(geometry.slots * geometry.stride + geometry.tail, PAGE);

    let mut address = None;
    let receiver = TurnLock::new((None, None));
    let mut work = Work::<2>::new();
    for value in [37, 52] {
        let mut plan = work.plan();
        plan.insert(&receiver).unwrap();
        pool.plan(&mut plan).unwrap();
        plan.execute(Fast, |held| {
            pool.reserve_held(held, &receiver, |record| &mut record.0)
                .unwrap();
        })
        .unwrap();

        let mut plan = work.plan();
        plan.insert(&receiver).unwrap();
        let reserved = plan
            .execute(Fast, |held| {
                held.access(&receiver).unwrap().0.take().unwrap()
            })
            .unwrap();
        let prepared = reserved.initialize(Stable::new(TurnLock::new(value)));
        let mut plan = work.plan();
        plan.insert(&receiver).unwrap();
        pool.plan(&mut plan).unwrap();
        plan.execute(Fast, |held| {
            prepared
                .publish_held(held, &receiver, |record| &mut record.1)
                .ok()
                .unwrap();
        })
        .unwrap();

        let mut plan = work.plan();
        plan.insert(&receiver).unwrap();
        let object = plan
            .execute(Fast, |held| {
                held.access(&receiver).unwrap().1.take().unwrap()
            })
            .unwrap();
        let original = object.stabilize();
        let reference = reference(&original);
        let entry = reference.get();
        let current = ptr::from_ref(entry).addr();
        assert!((GROUP.start as usize..GROUP.end as usize).contains(&current));
        assert_eq!(*address.get_or_insert(current), current);
        let mut object_work = Work::<1>::new();
        let mut plan = object_work.plan();
        plan.insert(entry).unwrap();
        assert_eq!(
            plan.execute(Fast, |held| *held.access(entry).unwrap()),
            Ok(value)
        );
        let detached = original.detach();
        let mut reclaim = None;
        let (detached, error) = detached.claim(&mut reclaim).err().unwrap();
        assert_eq!(error, Error::References);
        drop(reference);
        detached.claim(&mut reclaim).ok().unwrap();
        reclaim.unwrap().destroy().ok().unwrap();
        assert_eq!(
            pool.release_destroyed(&mut ReleaseCursor::default(), 1),
            Ok(ReleaseProgress::Released)
        );
    }
    assert_eq!(pool.capacity().unwrap().free, geometry.slots);
    assert_eq!(source.budget(Fast), Ok((PAGE as u64, 0)));
    let owner = allowance.as_ref().unwrap();
    assert_eq!(owner.balance().unwrap().occupied, PAGE as u64);
    assert_eq!(pool.shut(group), Ok(GROUP));
    assert_eq!(pool.capacity().unwrap().bytes, PAGE);
    let Retirement::Shutting(group, GROUP) = pool.retirement(&mut 0, 1).unwrap() else {
        panic!("shut group missing from the Pool directory");
    };
    // Bare RV64 has no active translation for the group, and the original
    // consumer has released every software reference. No device used it.
    unsafe { pool.begin_return(group, Fast) }.unwrap();
    let Retirement::Returning(group) = pool.retirement(&mut 0, 1).unwrap() else {
        panic!("returning group missing from the Pool directory");
    };
    let result = source.finish_return(supply.retirement, Fast).unwrap();
    assert_eq!(source.acknowledge(supply.retirement), Err(SourceError::Protection));
    assert_eq!(pool.finish_return(group, Fast), Ok(result));
    assert_eq!(result.allocation, supply.allocation);
    assert_eq!(result.physical, GROUP);
    assert_eq!(pool.finish_return(group, Fast), Ok(result));
    assert_eq!(source.acknowledge(supply.retirement), Err(SourceError::Stale));
    assert_eq!(owner.balance().unwrap().occupied, 0);
    assert_eq!(pool.capacity().unwrap().bytes, 0);
    boot::write("PASS typed-pool mapped=1 slots=1 reuse=1 joint=1 stable=1 refund=1\n");
}
