use kernel::{
    access::MappedRange,
    allocator::{
        Allocation, Allocator, Allowance, Config, Initial, MappedStorage,
        pool::{GroupId, GroupSlot, Pool},
    },
    entry::boot::{Boot, Physical},
};

use super::memory::{mapped, retirement};

pub(super) const PAGE: usize = 4096;
const BASE: u64 = 0x8420_0000;

const ROOT: Physical = Physical {
    start: BASE,
    end: BASE + PAGE as u64,
};
pub(super) const GROUP: Physical = Physical {
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

/// Call once during the guest's exclusive boot, before publishing the Pool.
pub(super) unsafe fn install<T>(
    boot: &Boot<'_>,
    directory: &'static mut [GroupSlot<'static, T>],
    allocation: &mut Option<Allocation>,
    allowance: &mut Option<Allowance>,
) -> (Pool<'static, T>, GroupId, &'static Allocator) {
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
    let mut pool = Pool::new(directory).unwrap();
    let mapping = unsafe { MappedRange::new(GROUP, mapped(GROUP)) }.unwrap();
    let mut paired = Some(
        unsafe { MappedStorage::prepare(allocation.take().unwrap(), mapping) }
            .ok()
            .unwrap(),
    );
    let group = unsafe { pool.install(&mut paired) }.unwrap();

    assert!(allocation.is_none() && paired.is_none());
    (pool, group, source)
}
