use core::{mem::MaybeUninit, ptr};

use kernel::{
    allocator::{
        Allocation, AllocationId, Allocator, Allowance, Candidate, Config, Construction,
        Error as SourceError, Initial,
        RamPurpose::Metadata,
        Request, ReturnId, ReturnReceipt, Returned, Search,
        pool::{Error as PoolError, GroupId, GroupSlot, Occupied, Pool, Retirement, Stable},
    },
    entry::boot::{self, Boot, MappedRange, Physical},
    pend::{Error, Handle, Initial as InitialPend, PendSpace, Revoker},
    sync::Acquisition::Fast,
};

use super::{
    memory::{mapped, retirement},
    pool::reference,
};

const PAGE: u64 = 4096;
const BASE: u64 = 0x8430_0000;
const CONFIG: Config = Config {
    granule: PAGE,
    maximum: 8 * PAGE,
    regions: 1,
    grains: 8,
    roots: 4,
    // The permanent PendSpace occupies one slot. Both ordinary allocations
    // must use the same remaining slot, each with its own generation.
    allocations: 2,
    returns: 2,
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
static FIRST: Construction = Construction::new();
static SECOND: Construction = Construction::new();
static mut MATERIAL: Option<Allocation> = None;
static mut RECEIPT: Option<ReturnReceipt> = None;

type Objects = Pool<'static, Stable<u64>>;
static mut OBJECT_GROUPS: [GroupSlot<'static, Stable<u64>>; 1] = [GroupSlot::EMPTY];
static mut OBJECTS: MaybeUninit<Objects> = MaybeUninit::uninit();

fn candidate(source: &'static Allocator, physical: Physical) -> Candidate {
    let request = Request::new(PAGE, PAGE, physical, None, Metadata).unwrap();
    let mut lookup = source.lookup(request, None).unwrap();
    loop {
        match lookup.step().unwrap() {
            Search::Candidate(candidate) => return candidate,
            Search::Pending => {}
            Search::NoRange => panic!("initial Pend has no metadata candidate"),
        }
    }
}

fn issue(
    handle: Handle,
    candidate: Candidate,
    construction: &'static Construction,
) -> AllocationId {
    let id = handle.reserve(candidate, construction, Fast).unwrap();
    assert_eq!(handle.issue(construction, Fast), Ok(id));
    id
}

fn retire_group(
    material: &mut Option<Allocation>,
    physical: Physical,
) -> (&'static Objects, ReturnId) {
    assert_eq!(material.as_ref().unwrap().physical(), physical);
    let mut pool = Pool::new(unsafe { &mut *(&raw mut OBJECT_GROUPS) }).unwrap();
    let group = unsafe { pool.install(material, &mut Some(mapped(physical))) }.unwrap();
    assert!(material.is_none());
    let pool = unsafe { (&mut *(&raw mut OBJECTS)).write(pool) };
    let supply = pool.supply(group).unwrap();
    let mut reservation = None;
    pool.reserve(&mut reservation).unwrap();
    let prepared = reservation.unwrap().initialize(Stable::new(0x1234_u64));
    let mut object = None;
    prepared.publish(&mut object).ok().unwrap();
    let owner = object.unwrap().stabilize();
    let reference = reference(&owner);
    let detached = owner.detach();
    let value = ptr::from_ref(reference.get());
    assert!((physical.start as usize..physical.end as usize).contains(&value.addr()));
    assert_eq!(unsafe { value.read_volatile() }, 0x1234);
    assert_eq!(pool.shut(group), Err(PoolError::Busy));
    let mut reclamation = None;
    let (detached, error) = detached.claim(&mut reclamation).err().unwrap();
    assert_eq!(error, PoolError::References);
    drop(reference);
    detached.claim(&mut reclamation).ok().unwrap();
    reclamation
        .unwrap()
        .destroy()
        .ok()
        .unwrap()
        .release()
        .ok()
        .unwrap();
    assert_eq!(pool.shut(group), Ok(physical));
    assert_eq!(pool.capacity().unwrap().bytes, PAGE as usize);

    // All software references ended. This bare guest never enabled a CPU
    // translation for the page, and no device or other Hart used it.
    unsafe { pool.begin_return(group, Fast) }.unwrap();
    (pool, supply.retirement)
}

fn recover_group(pool: &Objects) -> (GroupId, Returned) {
    let mut cursor = 0;
    let Retirement::Returning(group) = pool.retirement(&mut cursor, 1).unwrap() else {
        panic!("no returning group at the original directory");
    };
    let refunded = pool.finish_return(group, Fast).unwrap();
    cursor = 0;
    assert_eq!(pool.retirement(&mut cursor, 1), Ok(Retirement::End));
    assert_eq!(pool.capacity().unwrap().bytes, 0);
    (group, refunded)
}

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

    let physical = pages(3, 4);
    let material = unsafe { &mut *(&raw mut MATERIAL) };
    let first = issue(handle, candidate(source, physical), &FIRST);
    // The guest's durable field receives the material before handing it to the Pool.
    unsafe { FIRST.take(material) }.unwrap();
    assert_eq!(material.as_ref().unwrap().id(), first);
    assert_eq!(allowance.balance().unwrap().occupied, 2 * PAGE);
    let (objects, old_return) = retire_group(material, physical);
    assert_eq!(allowance.balance().unwrap().occupied, 2 * PAGE);

    // The original consumer has ended. Recovery finds the handed-off group
    // using the persistent bounded directory and settles with no spending role.
    let (retiring, refunded) = recover_group(objects);
    assert_eq!(refunded.allocation, first);
    assert_eq!(refunded.physical, physical);
    assert_eq!(allowance.balance(), Ok(original));

    let replacement = candidate(source, physical);
    let second = issue(handle, replacement, &SECOND);
    assert_ne!(first, second);
    assert_eq!(objects.finish_return(retiring, Fast), Ok(refunded));
    assert_eq!(allowance.balance().unwrap().occupied, 2 * PAGE);
    revoker.revoke().unwrap();
    assert_eq!(
        handle.reserve(replacement, &SECOND, Fast),
        Err(Error::Revoked)
    );
    handle.release().unwrap();
    assert_eq!(handle.release(), Err(Error::Exhausted));
    assert_eq!(revoker.revoked(), Ok(true));

    // The issued material remains in the original receiver through revocation
    // and release. Taking it needs only the original consumer's durable field.
    unsafe { SECOND.take(material) }.unwrap();
    let allocation = material.take().unwrap();
    assert_eq!(allocation.id(), second);
    assert_eq!(allocation.physical(), physical);
    {
        let byte = unsafe { mapped(allocation.physical()) }[0].as_mut_ptr();
        unsafe {
            byte.write_volatile(0x5a);
            assert_eq!(byte.read_volatile(), 0x5a);
        }
    }

    // The bare view is no longer in use. No other Hart or device accessed it.
    let receipt = unsafe { &mut *(&raw mut RECEIPT) };
    let retired = unsafe { allocation.retire() };
    unsafe { retired.begin_return(receipt) }.ok().unwrap();
    let receipt = receipt.as_ref().unwrap();
    // The current receipt is Pending and can settle. A Reserved receipt
    // would also report Stale and cannot distinguish the two generations.
    assert_eq!(
        source.finish_return(old_return, Fast),
        Err(SourceError::Stale)
    );
    assert_eq!(allowance.balance().unwrap().occupied, 2 * PAGE);
    let returned = receipt.finish(Fast).unwrap();
    assert_eq!(returned.allocation, second);
    assert_eq!(returned.physical, physical);
    assert_eq!(allowance.balance(), Ok(original));
    assert_eq!(
        source.finish_return(old_return, Fast),
        Err(SourceError::Stale)
    );
    assert_eq!(receipt.finish(Fast), Ok(returned));
    receipt.acknowledge().unwrap();
    assert_eq!(allowance.balance(), Ok(original));
    boot::write(
        "PASS bootstrap-foundation pend=1 pool=1 retained=1 refund=1 generation=1 stale=1 revoked=1\n",
    );
}
