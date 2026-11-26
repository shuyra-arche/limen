use core::mem::MaybeUninit;

use kernel::{
    allocator::{
        Allocation, Allowance, Config, Construction, Initial,
        RamPurpose::Metadata,
        Request, Search,
        pool::{GroupSlot, Occupied, Pool},
    },
    entry::boot::{self, Boot, MappedRange, Physical},
    pend::{Error, PendSpace},
    sync::{Acquisition::Fast, TurnLock, Work},
};

use super::memory::{mapped, retirement};

const PAGE: u64 = 4096;
const BASE: u64 = 0x8430_0000;
const CONFIG: Config = Config {
    granule: PAGE,
    maximum: 8 * PAGE,
    regions: 1,
    grains: 8,
    roots: 4,
    allocations: 3,
    returns: 4,
    allowances: 1,
    edits: 64,
};

const fn pages(first: u64, end: u64) -> Physical {
    Physical {
        start: BASE + first * PAGE,
        end: BASE + end * PAGE,
    }
}

static mut DIRECTORY: [GroupSlot<'static, PendSpace>; 1] = [GroupSlot::EMPTY];
static mut STORAGE: MaybeUninit<Pool<'static, PendSpace>> = MaybeUninit::uninit();
static mut INITIAL: Option<Allocation> = None;
static mut ALLOWANCE: Option<Allowance> = None;
static mut SPACE: Option<Occupied<'static, 'static, PendSpace>> = None;
static DESTINATION: TurnLock<Construction> = TurnLock::new(Construction::new());

pub(super) fn check(boot: &Boot<'_>) {
    let evidence = retirement(boot, pages(0, 6));
    let mut scratch = [None; 1];
    let mut regions = boot.regions(PAGE, Some(&evidence), &mut scratch).unwrap();
    let backing = pages(0, 2);
    let mut mapping = Some(unsafe { MappedRange::new(backing, mapped(backing)) }.unwrap());
    let material = unsafe { &mut *(&raw mut INITIAL) };
    let allowance = unsafe { &mut *(&raw mut ALLOWANCE) };
    let group = pages(2, 3);
    let source = unsafe {
        regions.allocator(
            &mut mapping,
            CONFIG,
            3 * PAGE,
            allowance,
            &mut [Initial::new(group, material)],
        )
    }
    .unwrap()
    .unwrap();
    let allowance = allowance.as_ref().unwrap();
    let original = allowance.balance().unwrap();
    assert_eq!(original.occupied, PAGE);

    let mut pool = Pool::new(unsafe { &mut *(&raw mut DIRECTORY) }).unwrap();
    unsafe { pool.install(material, &mut Some(mapped(group))) }.unwrap();
    let pool = unsafe { (&mut *(&raw mut STORAGE)).write(pool) };
    let mut reservation = None;
    pool.reserve(&mut reservation).unwrap();
    let prepared = reservation.unwrap().initialize(PendSpace::new());
    let receiver = unsafe { &mut *(&raw mut SPACE) };
    prepared.publish(receiver).ok().unwrap();
    let space = receiver.as_ref().unwrap().get();
    unsafe { space.publish_initial(allowance, PAGE, PAGE, Fast) }.unwrap();
    let handle = space.initial().unwrap();

    let request = Request::new(PAGE, PAGE, pages(3, 4), None, Metadata).unwrap();
    let mut lookup = source.lookup(request, None).unwrap();
    let candidate = loop {
        match lookup.step().unwrap() {
            Search::Candidate(candidate) => break candidate,
            Search::Pending => {}
            Search::NoRange => panic!("initial Pend has no metadata candidate"),
        }
    };
    let id = handle.reserve(candidate, &DESTINATION, Fast).unwrap();
    assert_eq!(handle.issue(&DESTINATION, Fast), Ok(id));
    assert_eq!(allowance.balance().unwrap().occupied, 2 * PAGE);
    space.revoke().unwrap();
    assert_eq!(
        handle.reserve(candidate, &DESTINATION, Fast),
        Err(Error::Revoked)
    );
    handle.release().unwrap();
    assert_eq!(space.initial().err(), Some(Error::Exhausted));

    let mut work = Work::<1>::new();
    let mut plan = work.plan();
    plan.insert(&DESTINATION).unwrap();
    let allocation = plan
        .execute(Fast, |held| unsafe {
            held.access(&DESTINATION).unwrap().take()
        })
        .unwrap()
        .unwrap();
    let physical = allocation.physical();
    assert_eq!(physical, pages(3, 4));
    // Use the issued material through the fixture's canonical bare mapping.
    // The reference ends here and no device or other hart used the page.
    let byte = &mut unsafe { mapped(physical) }[0];
    byte.write(0x5a);
    assert_eq!(unsafe { byte.assume_init_read() }, 0x5a);
    let mut receipt = None;
    source
        .begin_return(unsafe { allocation.retire() }, &mut receipt)
        .ok()
        .unwrap();
    let receipt = receipt.unwrap();
    assert_eq!(source.finish_return(receipt, Fast).unwrap().allocation, id);
    source.acknowledge(receipt).unwrap();
    assert_eq!(allowance.balance(), Ok(original));
    boot::write("PASS initial-pend installed=1 spend=1 revoked=1 refund=1\n");
}
