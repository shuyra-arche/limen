use core::{mem::MaybeUninit, ptr, slice};

use kernel::{
    allocator::{
        Allocation, Allowance, Config, Error, Grant, Initial, Pending, RamPurpose::Metadata,
        Request, ReturnId, Search,
    },
    entry::boot::{self, Boot, Condition, MappedRange, Physical, Retirement},
    sync::Acquisition::{Fast, Slow},
};

const PAGE: u64 = 4096;
const BASE: u64 = 0x8400_0000;
const CONFIG: Config = Config {
    granule: PAGE,
    maximum: 16 * PAGE,
    regions: 3,
    grains: 16,
    roots: 8,
    allocations: 4,
    returns: 6,
    allowances: 2,
    edits: 128,
};

static mut RECEIVER: Option<Allocation> = None;
static mut ALLOWANCE: Option<Allowance> = None;
static mut RECEIPT: Option<ReturnId> = None;

const fn pages(first: u64, last: u64) -> Physical {
    Physical {
        start: BASE + first * PAGE,
        end: BASE + last * PAGE,
    }
}

// The boot guest reserves this test-only RAM window in the bare virt fixture.
// It is outside the image, both startup stacks, OpenSBI and the retained DTB.
// No minor or DMA-capable guest driver has run and satp is still zero. The
// resident stack is permanent for this guest and stays excluded.
pub(super) fn retirement(boot: &Boot<'_>, physical: Physical) -> Retirement {
    let exclusions = boot.exclusions();
    assert!(exclusions.entries().all(|entry| {
        entry.physical.end <= physical.start || entry.physical.start >= physical.end
    }));
    assert!(
        boot.dtb_memory()
            .unwrap()
            .ram()
            .iter()
            .any(|ram| { ram.start <= physical.start && physical.end <= ram.end })
    );

    let mut retirement = Retirement::new(physical).unwrap();
    for condition in [
        Condition::RuntimeStack,
        Condition::ReadersGone,
        Condition::TranslationRetired,
        Condition::DevicesIsolated,
    ] {
        unsafe { retirement.record(condition) };
    }
    retirement
}

pub(super) fn check(boot: &Boot<'_>) {
    let scope = pages(0, 13);
    let backing = pages(4, 6);
    let evidence = retirement(boot, scope);
    let mut scratch = [None; 3];
    let mut regions = boot.regions(PAGE, Some(&evidence), &mut scratch).unwrap();
    assert!(regions.entries().map(|entry| entry.physical).eq([scope]));
    assert!(
        unsafe { regions.allocator(&mut None, CONFIG, 0, &mut None, &mut []) }
            .unwrap()
            .is_none()
    );

    let address = ptr::with_exposed_provenance_mut::<MaybeUninit<u8>>(backing.start as usize);
    // Bare RV64 gives this reserved extent a canonical identity mapping. The
    // single boot owner has not assigned it to any other object or source.
    let storage =
        unsafe { slice::from_raw_parts_mut(address, (backing.end - backing.start) as usize) };
    let mut mapping = Some(unsafe { MappedRange::new(backing, storage) }.unwrap());
    let receiver = unsafe { &mut *(&raw mut RECEIVER) };
    let allowance_receiver = unsafe { &mut *(&raw mut ALLOWANCE) };
    // Simulate loss of the transient boot result. All obligations have
    // already reached the original permanent receiving positions.
    assert!(
        unsafe {
            regions.allocator(
                &mut mapping,
                CONFIG,
                10 * PAGE,
                allowance_receiver,
                &mut [Initial::new(pages(0, 3), receiver)],
            )
        }
        .unwrap()
        .is_some()
    );
    let allowance = allowance_receiver.take().unwrap();
    let source = allowance.source();
    assert!(mapping.is_none() && regions.entries().next().is_none());
    assert_eq!(source.root(), backing);
    assert_eq!(source.budget(Slow), Ok((11 * PAGE, PAGE)));

    let mut lookup = source
        .lookup(
            Request::new(2 * PAGE, PAGE, pages(0, 4), None, Metadata).unwrap(),
            None,
        )
        .unwrap();
    loop {
        match lookup.step().unwrap() {
            Search::Pending => {}
            Search::NoRange => break,
            Search::Candidate(_) => panic!("initial occupied material was exposed as free"),
        }
    }

    let later = pages(16, 20);
    let mut absent = source
        .lookup(
            Request::new(PAGE, PAGE, later, None, Metadata).unwrap(),
            None,
        )
        .unwrap();
    let evidence = retirement(boot, later);
    let regions = boot.regions(PAGE, Some(&evidence), &mut scratch).unwrap();
    let description = regions.entries().next().unwrap();
    assert_eq!(description.physical, later);
    let mut grant = unsafe { Grant::new(source, description, &evidence) }.unwrap();
    let added = grant.admit(Fast).unwrap();
    assert_eq!(grant.admit(Slow), Ok(added));
    loop {
        match absent.step() {
            Ok(Search::Pending) => {}
            Err(Error::Changed) => break,
            _ => panic!("source negative survived publication"),
        }
    }

    let window = Physical {
        start: later.start + 1,
        end: later.end,
    };
    let request = Request::new(PAGE, 1, window, None, Metadata).unwrap();
    let Search::Candidate(aligned) = source.lookup(request, Some(added)).unwrap().step().unwrap()
    else {
        panic!("the new Region has an eligible aligned grain");
    };
    assert_eq!(aligned.physical(), pages(17, 18));

    let allocation = receiver.take().unwrap();
    let region = allocation.region();
    // This owner did not expose the allocation or issue any actual access.
    let receipt = unsafe { &mut *(&raw mut RECEIPT) };
    let _lost = source.begin_return(unsafe { allocation.retire() }, receipt);
    let Pending::Found(pending) = source.pending(region, &mut 0, 64).unwrap() else {
        panic!("lost response made the return undiscoverable");
    };
    allowance.shut(Some(added), Fast).unwrap();
    let done = source.finish_return(pending, Slow).unwrap();
    let receipt = receipt.take().unwrap();
    assert_eq!(receipt, pending);
    assert_eq!(done.physical, pages(0, 3));
    assert_eq!(source.budget(Fast), Ok((15 * PAGE, 15 * PAGE)));
    allowance.reclaim(Slow).ok().unwrap();
    assert_eq!(source.finish_return(receipt, Fast), Ok(done));
    source.acknowledge(receipt).unwrap();
    assert_eq!(source.finish_return(receipt, Fast), Err(Error::Stale));
    boot::write("PASS allocator source=1 negative=1 delayed=1 return=1\n");
}
