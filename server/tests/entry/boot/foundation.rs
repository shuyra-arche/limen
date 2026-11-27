use core::{mem::MaybeUninit, ptr};
use kernel::metal::platform::boot::treaty as boot_diagnostic;

use kernel::{
    basis::pend::{Error, Handle, Initial as InitialPend, PendSpace, Revoker},
    entry::boot::{Boot, Retirement as PhysicalRetirement},
    metal::{
        memory::access::treaty::ram::MappedRange,
        memory::allocator::treaty::{
            Allocation, AllocationConstruction, AllocationId, Allocator, Allowance,
            BudgetObservation, BudgetProgress, Cancellation, Candidate, Config,
            Error as SourceError, Initial, LookupObservation, MappedStorage, Pending,
            RamPurpose::Metadata,
            Request, ReturnId, ReturnReceipt, Returned, Search,
            pool::{
                Error as PoolError, GroupId, GroupReceipt, GroupSlot, Growth, Occupied, Pool,
                Reference, Retirement, Stable, StableOwner,
            },
        },
        sync::treaty::TurnMode::Fast,
    },
};

use super::memory::{extent, mapped};

#[path = "../../metal/memory/mmu/internal/table.rs"]
mod table;

use limen_treaty::model::{Alignment, ByteLength, PhysicalAddress, PhysicalRange};

const PAGE: u64 = 4096;
pub(super) const PAGES: usize = 6;
const CONFIG: Config = Config {
    granule: ByteLength::new(PAGE),
    maximum: ByteLength::new(8 * PAGE),
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

static mut SPACE_GROUP_RECEIPT: Option<GroupReceipt> = None;
static mut DIRECTORY: [GroupSlot<'static, PendSpace>; 1] = [GroupSlot::EMPTY];
static mut STORAGE: MaybeUninit<Pool<'static, PendSpace>> = MaybeUninit::uninit();
static mut INITIAL: Option<Allocation> = None;
static mut ALLOWANCE: Option<Allowance> = None;
static mut SPACE: Option<Occupied<'static, 'static, PendSpace>> = None;
static mut SPENDING: Option<Handle> = None;
static mut CLOSING: Option<Revoker> = None;
static ABANDONED: AllocationConstruction = AllocationConstruction::new();
static mut FRESH: MaybeUninit<Growth> = MaybeUninit::uninit();
static mut ABORTED_GROUP: MaybeUninit<Growth> = MaybeUninit::uninit();
static HISTORY: AllocationConstruction = AllocationConstruction::new();
static SECOND: AllocationConstruction = AllocationConstruction::new();
static mut MATERIAL: Option<Allocation> = None;
static mut HISTORY_RECEIPT: Option<ReturnReceipt> = None;
static mut RECEIPT: Option<ReturnReceipt> = None;

type Payload = [u64; 32];
type Objects = Pool<'static, Stable<Payload>>;
static mut OBJECT_GROUP_RECEIPT: Option<GroupReceipt> = None;
static mut OBJECT_GROUPS: [GroupSlot<'static, Stable<Payload>>; 2] =
    [const { GroupSlot::EMPTY }; 2];
static mut OBJECTS: MaybeUninit<Objects> = MaybeUninit::uninit();

pub(super) fn reference<'pool, T: Sync>(owner: &StableOwner<'pool, '_, T>) -> Reference<'pool, T> {
    for _ in 0..16 {
        match owner.reference() {
            Ok(reference) => return reference,
            Err(PoolError::Busy) => {}
            Err(error) => panic!("unexpected retain error: {error:?}"),
        }
    }
    panic!("retain stayed Busy for 16 attempts");
}

pub(super) fn inspect_budget(source: &Allocator) -> Result<(ByteLength, ByteLength), SourceError> {
    let mut observations = [BudgetObservation::EMPTY; 3];
    let mut budget = source.budget(&mut observations)?;
    loop {
        match budget.step()? {
            BudgetProgress::Pending => {}
            BudgetProgress::Complete(managed, unassigned) => return Ok((managed, unassigned)),
        }
    }
}

fn search(source: &'static Allocator, length: u64, physical: PhysicalRange) -> Option<Candidate> {
    let request = Request::new(
        ByteLength::new(length),
        Alignment::new(PAGE).unwrap(),
        physical,
        None,
        Metadata,
    )
    .unwrap();
    let mut lookup_observations = [LookupObservation::EMPTY; 3];
    let mut lookup = source
        .lookup(request, None, &mut lookup_observations)
        .unwrap();
    loop {
        match lookup.step().unwrap() {
            Search::Candidate(candidate) => return Some(candidate),
            Search::Pending => {}
            Search::NoRange => return None,
        }
    }
}

fn issue(
    handle: Handle,
    candidate: Candidate,
    construction: &'static AllocationConstruction,
) -> AllocationId {
    let id = handle.reserve(candidate, construction, Fast).unwrap();
    assert_eq!(handle.issue(construction, Fast), Ok(id));
    id
}

fn check_read_write(mapping: &mut MappedRange<'_>, first: u8, last: u8) {
    let physical = mapping.physical();
    let bytes = mapping.bytes();
    let (head, tail) = bytes.split_first_mut().unwrap();
    let head = head.as_mut_ptr();
    let tail = tail.last_mut().unwrap().as_mut_ptr();
    assert_eq!(head.addr() as u64, physical.start().get());
    assert_eq!(tail.addr() as u64, physical.end().get() - 1);
    // Each probe stays in the current private borrow. Check both stores
    // before returning either byte to any subsequent owner.
    unsafe {
        head.write_volatile(first);
        tail.write_volatile(last);
        assert_eq!(head.read_volatile(), first);
        assert_eq!(tail.read_volatile(), last);
    }
}

fn retire_group(
    pool: &'static Objects,
    prepared: GroupId,
    material: &mut Option<Allocation>,
    physical: PhysicalRange,
) -> (&'static Objects, ReturnId) {
    assert_eq!(material.as_ref().unwrap().physical(), physical);
    let mut mapping = unsafe { mapped(physical) };
    check_read_write(&mut mapping, 0x41, 0x42);
    let mut paired = Some(
        unsafe { MappedStorage::prepare(material.take().unwrap(), mapping) }
            .ok()
            .unwrap(),
    );
    let receipt = unsafe { &mut *(&raw mut OBJECT_GROUP_RECEIPT) };
    // The independent directory position exists, but this page already
    // consumes the entire physical budget through its original preparation.
    assert_eq!(
        unsafe { pool.install(&mut paired, receipt) },
        Err(PoolError::Directory)
    );
    assert!(paired.is_some() && receipt.is_none());
    let group = unsafe { pool.install_prepared(prepared, &mut paired, receipt) }.unwrap();
    assert_eq!(prepared, group);
    assert!(material.is_none() && paired.is_none());
    let supply = pool.supply(group).unwrap();
    let mut reservation = None;
    pool.reserve(&mut reservation).unwrap();
    let prepared = reservation.unwrap().initialize(Stable::new([0x1234; 32]));
    let mut object = None;
    prepared.publish(&mut object).ok().unwrap();
    let owner = object.unwrap().stabilize();
    let reference = reference(&owner);
    let detached = owner.detach();
    let value = ptr::from_ref(&reference.get()[0]);
    assert!(physical.contains(PhysicalAddress::new(value.addr() as u64)));
    assert_eq!(unsafe { value.read_volatile() }, 0x1234);
    // The payload alone is 256 bytes, so 16 records cover every slot in a page.
    let mut occupied = [const { None }; 16];
    assert!(supply.geometry.slots <= occupied.len());
    let occupied = &mut occupied[..supply.geometry.slots - 1];
    for slot in &mut *occupied {
        pool.reserve(slot).unwrap();
    }
    assert_eq!(pool.reserve(&mut None), Err(PoolError::Full));
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
    assert_eq!(pool.capacity().unwrap().free, 1);
    for slot in occupied {
        slot.take().unwrap().cancel().ok().unwrap();
    }
    assert_eq!(pool.shut(group), Ok(physical));
    assert_eq!(pool.reserve(&mut None), Err(PoolError::Full));
    assert_eq!(pool.capacity().unwrap().bytes, PAGE as usize);

    // All software references ended. The permanent identity view is
    // still available. No other Hart or device used the page.
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

fn exhaust_history(
    allowance: &Allowance,
    handle: Handle,
    physical: PhysicalRange,
    material: &mut Option<Allocation>,
) {
    let source = allowance.source();
    let before = allowance.balance().unwrap();
    let candidate = search(source, PAGE, physical).unwrap();
    let id = issue(handle, candidate, &HISTORY);
    // This consumer keeps the issued page private and never uses its bytes.
    unsafe { HISTORY.take(material) }.unwrap();
    let allocation = material.take().unwrap();
    let region = allocation.region();
    let retired = unsafe { allocation.retire() };
    let receipt = unsafe { &mut *(&raw mut HISTORY_RECEIPT) };
    // The recovery scan consumes no notification or immediate return result.
    let outcome = unsafe { retired.begin_return(receipt) };
    assert!(outcome.is_ok());
    let mut cursor = 0;
    let pending = (0..CONFIG.returns)
        .find_map(|_| match source.pending(region, &mut cursor, 1).unwrap() {
            Pending::Found(id) => Some(id),
            Pending::More => None,
            Pending::End => panic!("scan ended before finding the original return"),
        })
        .expect("original pending return missing from the bounded table");
    assert_eq!(source.finish_return(pending, Fast).unwrap().allocation, id);
    assert_eq!(allowance.balance(), Ok(before));

    let candidate = search(source, PAGE, physical).unwrap();
    assert_eq!(
        handle.reserve(candidate, &SECOND, Fast),
        Err(Error::Source(SourceError::ReturnCapacity))
    );
    assert_eq!(allowance.balance(), Ok(before));
    let receipt = receipt.take().unwrap();
    assert_eq!(receipt.id(), pending);
    receipt.acknowledge().unwrap();
}

/// # Safety
/// The platform fixture must own the whole isolated physical window and its
/// canonical writable identity view for the duration of the guest.
pub(super) unsafe fn check(boot: &Boot<'_>, start: u64, evidence: &PhysicalRetirement) {
    let pages = |first, end| extent(start + first * PAGE, start + end * PAGE);
    let scope = pages(0, PAGES as u64);
    let mut scratch = [None; 1];
    let mut regions = boot
        .regions(ByteLength::new(PAGE), Some(evidence), &mut scratch)
        .unwrap();
    assert!(regions.entries().map(|entry| entry.physical).eq([scope]));
    let backing = pages(0, 2);
    let mut mapping = Some(unsafe { mapped(backing) });
    let material = unsafe { &mut *(&raw mut INITIAL) };
    let allowance = unsafe { &mut *(&raw mut ALLOWANCE) };
    let group = pages(2, 3);
    assert_eq!(
        unsafe {
            regions.allocator(
                &mut mapping,
                CONFIG,
                PAGE - 1,
                allowance,
                &mut [Initial::new(group, material)],
            )
        }
        .err(),
        Some(SourceError::Capacity)
    );
    assert!(mapping.is_some() && material.is_none() && allowance.is_none());
    assert!(regions.entries().map(|entry| entry.physical).eq([scope]));
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
    let (managed, unassigned) = inspect_budget(source)
        .map(|(total, unassigned)| (total.get(), unassigned.get()))
        .unwrap();
    let root = source.root();
    assert!(mapping.is_none() && regions.entries().next().is_none());
    assert_eq!(root.start(), backing.start());
    assert!(backing.covers(root));
    assert_eq!(managed + root.length().get(), scope.length().get());
    assert_eq!(original.occupied().get(), PAGE);
    assert_eq!(managed, unassigned + original.capacity().get());

    let pool = Pool::new(unsafe { &mut *(&raw mut DIRECTORY) }).unwrap();
    let mapping = unsafe { mapped(group) };
    let mut paired = Some(
        unsafe { MappedStorage::prepare(material.take().unwrap(), mapping) }
            .ok()
            .unwrap(),
    );
    let receipt = unsafe { &mut *(&raw mut SPACE_GROUP_RECEIPT) };
    unsafe { pool.install(&mut paired, receipt) }.unwrap();
    assert!(material.is_none() && paired.is_none());
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
            ByteLength::new(PAGE),
            Alignment::new(PAGE).unwrap(),
            InitialPend { spending, closing },
            Fast,
        )
    }
    .unwrap();
    let handle = spending.as_ref().copied().unwrap();
    let revoker = closing.as_ref().unwrap();

    let physical = pages(4, 5);
    unsafe { table::check(boot, allowance, physical) };
    boot_diagnostic::write(
        "PASS table-page issuer=1 private=1 initialized=1 generation=1 refund=1\n",
    );
    let material = unsafe { &mut *(&raw mut MATERIAL) };
    let candidate = search(source, PAGE, physical).unwrap();
    let reserved = handle.reserve(candidate, &ABANDONED, Fast).unwrap();
    let mut cursor = 0;
    // Discovery cannot close the still-live preparation. Ordinary capacity
    // is full, and the same two slots must suffice for cancellation and reuse.
    for _ in 0..CONFIG.allocations {
        assert_eq!(
            source.cancel_closed(candidate.region(), &mut cursor, Fast),
            Ok(Cancellation::More)
        );
    }
    assert_eq!(
        source.cancel_closed(candidate.region(), &mut cursor, Fast),
        Ok(Cancellation::End)
    );
    assert_eq!(allowance.balance().unwrap().reserved().get(), PAGE);
    ABANDONED.shut().unwrap();
    let mut recovered = None;
    for _ in 0..CONFIG.allocations {
        match source
            .cancel_closed(candidate.region(), &mut cursor, Fast)
            .unwrap()
        {
            Cancellation::Cancelled(id) => assert_eq!(recovered.replace(id), None),
            Cancellation::More => {}
            Cancellation::End => panic!("premature end of recovery pass"),
        }
    }
    assert_eq!(
        source.cancel_closed(candidate.region(), &mut cursor, Fast),
        Ok(Cancellation::End)
    );
    assert_eq!(recovered, Some(reserved));
    assert_eq!(allowance.balance().unwrap(), original);
    let directory = unsafe { &mut *(&raw mut OBJECT_GROUPS) };
    let objects = Pool::bounded(directory, PAGE as usize).unwrap();
    let objects = unsafe { (&mut *(&raw mut OBJECTS)).write(objects) };
    let mut aborted =
        Some(unsafe { (&mut *(&raw mut ABORTED_GROUP)).write(Growth::new(physical)) });
    let mut fresh = Some(unsafe { (&mut *(&raw mut FRESH)).write(Growth::new(physical)) });
    let (old, abandoned) = objects.prepare(&mut aborted).unwrap();
    assert!(aborted.is_none());
    assert!(matches!(
        objects.prepare(&mut fresh),
        Err(PoolError::Directory)
    ));
    assert!(fresh.is_some());
    assert_eq!(objects.reserve(&mut None), Err(PoolError::Full));
    assert_eq!(objects.discard_empty(old), Err(PoolError::Busy));
    handle
        .reserve(search(source, PAGE, physical).unwrap(), abandoned, Fast)
        .unwrap();
    abandoned.shut().unwrap();
    assert_eq!(objects.discard_empty(old), Err(PoolError::Busy));
    abandoned.cancel(Fast).unwrap();
    let mut scan = 0;
    assert_eq!(
        objects.retirement(&mut scan, 1),
        Ok(Retirement::Preparing(old))
    );
    objects.discard_empty(old).unwrap();
    let (prepared, original_request) = objects.prepare(&mut fresh).unwrap();
    assert_ne!(prepared, old);
    assert_eq!(objects.discard_empty(old), Err(PoolError::Stale));
    let outside = search(source, PAGE, pages(3, 4)).unwrap();
    assert_eq!(
        handle.reserve(outside, original_request, Fast),
        Err(Error::Source(SourceError::Range))
    );
    assert_eq!(allowance.balance(), Ok(original));
    let first = issue(
        handle,
        search(source, PAGE, physical).unwrap(),
        original_request,
    );
    assert_eq!(objects.discard_empty(prepared), Err(PoolError::Busy));
    unsafe { original_request.take(material) }.unwrap();
    assert_eq!(material.as_ref().unwrap().id(), first);
    let charged = allowance.balance().unwrap();
    assert_eq!(charged.occupied().get(), 2 * PAGE);
    let free = search(source, PAGE, pages(3, 4)).unwrap();
    assert!(search(source, PAGE, pages(5, 6)).is_some());
    assert!(search(source, 2 * PAGE, pages(2, 6)).is_none());
    assert_eq!(
        handle.reserve(free, &SECOND, Fast),
        Err(Error::Source(SourceError::AllocationCapacity))
    );
    assert_eq!(allowance.balance(), Ok(charged));
    let (objects, old_return) = retire_group(objects, prepared, material, physical);
    assert_eq!(allowance.balance().unwrap().occupied().get(), 2 * PAGE);

    // The original consumer has ended. Recovery finds the handed-off group
    // using the persistent bounded directory and settles with no spending role.
    let (retiring, refunded) = recover_group(objects);
    assert_eq!(refunded.allocation, first);
    assert_eq!(refunded.physical, physical);
    assert_eq!(allowance.balance(), Ok(original));

    exhaust_history(allowance, handle, physical, material);
    let replacement = search(source, PAGE, physical).unwrap();
    let second = issue(handle, replacement, &SECOND);
    assert_ne!(first, second);
    assert_eq!(objects.finish_return(retiring, Fast), Ok(refunded));
    assert_eq!(allowance.balance().unwrap().occupied().get(), 2 * PAGE);
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
    allowance.shut(Some(allocation.region()), Fast).unwrap();
    let closed = allowance.balance().unwrap();
    assert_eq!(closed.occupied().get(), charged.occupied().get());
    assert_eq!(closed.available().get(), 0);
    {
        let mut mapping = unsafe { mapped(allocation.physical()) };
        check_read_write(&mut mapping, 0x5a, 0xa5);
    }

    // The identity view is no longer borrowed. No other Hart or device accessed it.
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
    assert_eq!(allowance.balance(), Ok(closed));
    let returned = receipt.finish(Fast).unwrap();
    assert_eq!(returned.allocation, second);
    assert_eq!(returned.physical, physical);
    assert_eq!(
        source.finish_return(old_return, Fast),
        Err(SourceError::Stale)
    );
    assert_eq!(receipt.finish(Fast), Ok(returned));
    receipt.acknowledge().unwrap();
    let remaining = allowance.balance().unwrap();
    assert_eq!(remaining.occupied().get(), PAGE);
    assert_eq!(remaining.available().get(), 0);
    assert_eq!(
        inspect_budget(source).map(|(total, unassigned)| (total.get(), unassigned.get())),
        Ok((managed, managed - PAGE))
    );
    boot_diagnostic::write(
        "PASS bootstrap-foundation pend=1 pool=1 retained=1 refund=1 generation=1 stale=1 revoked=1\n",
    );
    boot_diagnostic::write(
        "PASS foundation-pressure preflight=1 fragments=1 allocation=1 pool=1 history=1 rediscover=1 shut=1\n",
    );
    boot_diagnostic::write("PASS boot-mapped-storage identity=1 range=1 rw=1 reuse=1\n");
}
