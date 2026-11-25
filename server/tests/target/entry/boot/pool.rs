use core::{mem::MaybeUninit, ptr, slice};

use kernel::{
    allocator::{
        Allocation, Allowance, Config, Initial,
        pool::{GroupSlot, Pool},
    },
    entry::boot::{self, Boot, MappedRange, Physical},
    sync::{Acquisition::Fast, TurnLock, Work},
};

use super::allocator::retirement;

const PAGE: usize = 4096;
const BASE: u64 = 0x8420_0000;

const ROOT: Physical = Physical {
    start: BASE,
    end: BASE + PAGE as u64,
};
const GROUP: Physical = Physical {
    start: ROOT.end,
    end: ROOT.end + PAGE as u64,
};
const CONFIG: Config = Config {
    granule: PAGE as u64,
    maximum: PAGE as u64,
    regions: 1,
    grains: 1,
    roots: 1,
    allocations: 1,
    returns: 2,
    allowances: 1,
    edits: 2,
};

type Entry = TurnLock<usize>;

static mut GROUPS: [GroupSlot<'static, Entry>; 1] = [GroupSlot::EMPTY];
static mut TYPED: MaybeUninit<Pool<'static, Entry>> = MaybeUninit::uninit();
static mut ALLOCATION: Option<Allocation> = None;
static mut ALLOWANCE: Option<Allowance> = None;

pub(super) fn check(boot: &Boot<'_>) {
    let extent = Physical {
        start: ROOT.start,
        end: GROUP.end,
    };
    let evidence = retirement(boot, extent);
    let mut scratch = [None; 1];
    let mut regions = boot
        .regions(PAGE as u64, Some(&evidence), &mut scratch)
        .unwrap();
    // Bare RV64 has a canonical identity mapping for these disjoint, checked
    // RAM pages. Neither has an alias or a prior owner in this fixture.
    let mut backing = Some(unsafe { MappedRange::new(ROOT, mapped(ROOT)) }.unwrap());
    let allocation = unsafe { &mut *(&raw mut ALLOCATION) };
    let allowance = unsafe { &mut *(&raw mut ALLOWANCE) };
    let source = unsafe {
        regions.allocator(
            &mut backing,
            CONFIG,
            PAGE as u64,
            allowance,
            &mut [Initial::new(GROUP, allocation)],
        )
    }
    .unwrap()
    .unwrap();
    let key = allocation.as_ref().unwrap().id();
    let directory = unsafe { &mut *(&raw mut GROUPS) };
    let mut pool = Pool::new(directory).unwrap();
    let mut bytes = Some(unsafe { mapped(GROUP) });
    let group = unsafe { pool.install(allocation, &mut bytes) }.unwrap();
    let pool = unsafe { (&mut *(&raw mut TYPED)).write(pool) };

    let supply = pool.supply(group).unwrap();
    assert_eq!(supply.allocation, key);
    let geometry = supply.geometry;
    assert!(geometry.slots > 1 && geometry.alignment >= core::mem::align_of::<Entry>());
    assert_eq!(geometry.slots * geometry.stride + geometry.tail, PAGE);
    assert!(allocation.is_none() && bytes.is_none());

    let mut address = None;
    for value in [37, 52] {
        let mut reserved = None;
        pool.reserve(&mut reserved).unwrap();
        let mut owner = None;
        reserved
            .take()
            .unwrap()
            .initialize(TurnLock::new(value))
            .publish(&mut owner)
            .ok()
            .unwrap();
        let object = owner.take().unwrap();
        let entry = object.get();
        let current = ptr::from_ref(entry).addr();
        assert!((GROUP.start as usize..GROUP.end as usize).contains(&current));
        assert_eq!(*address.get_or_insert(current), current);
        let mut work = Work::<1>::new();
        let mut plan = work.plan();
        plan.insert(entry).unwrap();
        assert_eq!(
            plan.execute(Fast, |held| *held.access(entry).unwrap()),
            Ok(value)
        );
        object.destroy().ok().unwrap().release().ok().unwrap();
    }
    assert_eq!(pool.capacity().unwrap().free, geometry.slots);
    assert_eq!(source.budget(Fast), Ok((PAGE as u64, 0)));
    assert_eq!(
        allowance.as_ref().unwrap().balance().unwrap().occupied,
        PAGE as u64
    );
    boot::write("PASS typed-pool mapped=1 slots=1 reuse=1\n");
}

unsafe fn mapped(physical: Physical) -> &'static mut [MaybeUninit<u8>] {
    let base = ptr::with_exposed_provenance_mut(physical.start as usize);
    unsafe { slice::from_raw_parts_mut(base, (physical.end - physical.start) as usize) }
}
