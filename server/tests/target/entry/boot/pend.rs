use core::mem::MaybeUninit;

use kernel::{
    allocator::{
        Allocation, Allowance, Config, Construction, Initial,
        RamPurpose::Metadata,
        Request, ReturnReceipt, Search,
        pool::{GroupSlot, Occupied, Pool},
    },
    entry::boot::{self, Boot, MappedRange, Physical},
    pend::{Error, Handle, Initial as InitialPend, PendSpace, Revoker},
    sync::Acquisition::Fast,
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
static mut SPENDING: Option<Handle> = None;
static mut CLOSING: Option<Revoker> = None;
static DESTINATION: Construction = Construction::new();
static mut MATERIAL: Option<Allocation> = None;
static mut RECEIPT: Option<ReturnReceipt> = None;

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
    let spending = unsafe { &mut *(&raw mut SPENDING) };
    let closing = unsafe { &mut *(&raw mut CLOSING) };
    unsafe {
        space.publish_initial(
            allowance,
            PAGE,
            PAGE,
            InitialPend { spending, closing },
            Fast,
        )
    }
    .unwrap();
    let handle = spending.as_ref().copied().unwrap();
    let revoker = closing.as_ref().unwrap();

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
    revoker.revoke().unwrap();
    assert_eq!(
        handle.reserve(candidate, &DESTINATION, Fast),
        Err(Error::Revoked)
    );
    handle.release().unwrap();
    assert_eq!(handle.release(), Err(Error::Exhausted));
    assert_eq!(revoker.revoked(), Ok(true));

    let material = unsafe { &mut *(&raw mut MATERIAL) };
    unsafe { DESTINATION.take(material) }.unwrap();
    let allocation = material.take().unwrap();
    let physical = allocation.physical();
    assert_eq!(physical, pages(3, 4));
    // Use the issued material through the fixture's canonical bare mapping.
    // The reference ends here and no device or other hart used the page.
    let byte = unsafe { mapped(physical) }[0].as_mut_ptr();
    unsafe {
        byte.write_volatile(0x5a);
        assert_eq!(byte.read_volatile(), 0x5a);
    }
    let receipt = unsafe { &mut *(&raw mut RECEIPT) };
    let retired = unsafe { allocation.retire() };
    unsafe { retired.begin_return(receipt) }.ok().unwrap();
    let receipt = receipt.as_ref().unwrap();
    assert_eq!(receipt.finish(Fast).unwrap().allocation, id);
    receipt.acknowledge().unwrap();
    assert_eq!(allowance.balance(), Ok(original));
    boot::write("PASS initial-pend installed=1 spend=1 revoked=1 refund=1\n");
}
