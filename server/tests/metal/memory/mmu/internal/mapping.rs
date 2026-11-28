//! Common ordinary RAM Mapping lifecycle through a real installed local Nest.

use core::mem::MaybeUninit;

use kernel::{
    basis::{
        cell::{self, AccessPreparation, AccessStorage, CellConstruction, CellSlot, Rights},
        nest::{self, LocalNest, MappingPhase, MappingProgress},
        pend::{self, Initial as InitialPend, PendSpace},
    },
    metal::{
        memory::{
            access::treaty::ram::MappedRange,
            allocator::treaty::{
                AllocationConstruction, Allowance, LookupObservation, MappedStorage, RamPurpose,
                Request, Search,
            },
            mmu::treaty::{TableError, TableId, TableStore},
        },
        sync::treaty::TurnMode::Fast,
    },
};
use limen_treaty::model::{
    Alignment, ByteLength, ByteOffset, ByteRange, InternalAddress, InternalRange, PhysicalRange,
};

static DATA_SPENDING: PendSpace = PendSpace::for_cells();
static DATA_ALLOCATION: AllocationConstruction = AllocationConstruction::new();
static DATA_CONSTRUCTION: CellConstruction = CellConstruction::new();
static mut DATA_ACCESSES: AccessStorage<2> = AccessStorage::new();
static mut DATA_CELL: MaybeUninit<CellSlot> = MaybeUninit::uninit();
static FIRST_ACCESS: AccessPreparation = AccessPreparation::new();
static SECOND_ACCESS: AccessPreparation = AccessPreparation::new();

fn range(start: u64, end: u64) -> InternalRange {
    InternalRange::new(InternalAddress::new(start), InternalAddress::new(end)).unwrap()
}

/// The fixture retains the original RAM, compatible Normal identity
/// mapping and exclusive local native scope until all accesses settle.
pub(super) unsafe fn check(
    owner: &mut LocalNest<'_, '_>,
    allowance: &Allowance,
    physical: PhysicalRange,
    address: u64,
    supplied: &[(&'static TableStore, TableId)],
    mapped: unsafe fn(PhysicalRange) -> MappedRange<'static>,
    load: impl Fn(u64) -> u64,
    store: impl Fn(u64, u64),
) {
    const PAGE: u64 = 4096;
    const LENGTH: u64 = 2 * PAGE;
    assert_eq!(physical.length(), ByteLength::new(LENGTH));
    assert!(physical.start().get().is_multiple_of(LENGTH));
    let balance = allowance.balance().unwrap();
    let mut spending = None;
    let mut closing = None;
    unsafe {
        DATA_SPENDING.publish_initial(
            allowance,
            ByteLength::new(LENGTH),
            Alignment::new(LENGTH).unwrap(),
            InitialPend {
                spending: &mut spending,
                closing: &mut closing,
            },
            Fast,
        )
    }
    .unwrap();
    let spending = spending.unwrap();
    let request = Request::new(
        ByteLength::new(LENGTH),
        Alignment::new(LENGTH).unwrap(),
        physical,
        None,
        RamPurpose::Cell,
    )
    .unwrap();
    let mut observations = [LookupObservation::EMPTY; 1];
    let mut lookup = allowance
        .source()
        .lookup(request, None, &mut observations)
        .unwrap();
    let candidate = loop {
        match lookup.step().unwrap() {
            Search::Candidate(candidate) => break candidate,
            Search::Pending => {}
            Search::NoRange => panic!("original ordinary page is available"),
        }
    };
    assert_eq!(candidate.physical(), physical);
    let allocation = spending.reserve(candidate, &DATA_ALLOCATION, Fast).unwrap();
    assert_eq!(spending.issue(&DATA_ALLOCATION, Fast), Ok(allocation));
    let mut material = None;
    unsafe { DATA_ALLOCATION.take(&mut material) }.unwrap();
    let material = unsafe { MappedStorage::prepare(material.take().unwrap(), mapped(physical)) }
        .ok()
        .unwrap();
    let slot = unsafe {
        (&mut *(&raw mut DATA_CELL))
            .write(CellSlot::for_cells(&mut *(&raw mut DATA_ACCESSES)).unwrap())
    };
    let cell = DATA_CONSTRUCTION
        .publish(slot, spending, material, Fast)
        .ok()
        .unwrap();
    closing.unwrap().revoke().unwrap();
    spending.release().unwrap();
    let bytes = ByteRange::new(ByteOffset::new(0), ByteOffset::new(LENGTH)).unwrap();
    let virtual_range = range(address, address + 2 * LENGTH);
    let span = owner.reserve_span(virtual_range).unwrap();
    let first = DATA_CONSTRUCTION
        .prepare(cell, bytes, Rights::READ_WRITE, &FIRST_ACCESS, Fast)
        .unwrap();
    let second = DATA_CONSTRUCTION
        .prepare(cell, bytes, Rights::READ, &SECOND_ACCESS, Fast)
        .unwrap();
    let virtual_address = InternalAddress::new(address);
    let other_address = InternalAddress::new(address + LENGTH);
    assert!(!supplied.is_empty());
    assert_eq!(
        owner.missing_table(span, virtual_address, Fast),
        Ok(Some(supplied.len() - 1))
    );
    assert_eq!(
        unsafe { owner.map(span, virtual_address, &FIRST_ACCESS, first, Fast) },
        Err(nest::Error::Table(TableError::Scope))
    );
    assert_eq!(FIRST_ACCESS.observe(), Ok(Some(first)));
    let (last, last_id) = supplied[supplied.len() - 1];
    if supplied.len() > 1 {
        assert_eq!(
            unsafe { owner.supply_table(span, virtual_address, last, last_id, Fast) },
            Err(nest::Error::Table(TableError::Layout))
        );
        assert_eq!(last.descriptor(last_id, 0, Fast), Ok(0));
    }
    for (offset, &(store, id)) in supplied.iter().enumerate() {
        assert_eq!(
            owner.missing_table(span, virtual_address, Fast),
            Ok(Some(supplied.len() - offset - 1))
        );
        assert_eq!(
            unsafe {
                owner.supply_table(
                    span,
                    InternalAddress::new(address + 2 * LENGTH),
                    store,
                    id,
                    Fast,
                )
            },
            Err(nest::Error::Scope)
        );
        assert_eq!(store.descriptor(id, 0, Fast), Ok(0));
        unsafe { owner.supply_table(span, virtual_address, store, id, Fast) }.unwrap();
        assert_eq!(
            owner.missing_table(span, virtual_address, Fast),
            Err(nest::Error::Table(TableError::State))
        );
        assert_eq!(
            unsafe { owner.map(span, virtual_address, &FIRST_ACCESS, first, Fast) },
            Err(nest::Error::Table(TableError::State))
        );
        assert_eq!(FIRST_ACCESS.observe(), Ok(Some(first)));
        assert_eq!(store.descriptor(id, 0, Fast), Err(TableError::State));
        unsafe { owner.advance_supply(Fast) }.unwrap();
    }
    assert_eq!(owner.missing_table(span, virtual_address, Fast), Ok(None));
    assert_eq!(
        unsafe { owner.supply_table(span, virtual_address, last, last_id, Fast) },
        Err(nest::Error::Table(TableError::Occupied))
    );
    let mapping = unsafe { owner.map(span, virtual_address, &FIRST_ACCESS, first, Fast) }.unwrap();
    assert_eq!(FIRST_ACCESS.observe(), Ok(None));
    let accepted = owner.observe_mapping(mapping).unwrap();
    assert_eq!(accepted.phase, MappingPhase::Installing);
    assert_eq!(accepted.access, first);
    assert_eq!(accepted.rights, Rights::READ_WRITE);
    assert_eq!(accepted.range, range(address, address + LENGTH));
    assert_eq!(
        unsafe { owner.map(span, virtual_address, &SECOND_ACCESS, second, Fast) },
        Err(nest::Error::Conflict)
    );
    assert_eq!(
        unsafe { owner.map(span, other_address, &SECOND_ACCESS, second, Fast) },
        Err(nest::Error::Capacity)
    );
    assert_eq!(SECOND_ACCESS.observe(), Ok(Some(second)));

    // Cancel after the first real leaf was made, but before the second.
    // The full address range and the accepted physical use remain occupied.
    assert_eq!(
        unsafe { owner.advance_mapping(mapping, Fast) },
        Ok(MappingProgress::Pending)
    );
    assert_eq!(
        owner.observe_mapping(mapping).unwrap().phase,
        MappingPhase::Installing
    );
    assert_eq!(load(address), 0);
    owner.begin_unmap(mapping).unwrap();
    assert_eq!(
        unsafe { owner.advance_mapping(mapping, Fast) },
        Ok(MappingProgress::Pending)
    );
    assert_eq!(
        unsafe { owner.map(span, virtual_address, &SECOND_ACCESS, second, Fast) },
        Err(nest::Error::Conflict)
    );
    let mut cancelled = None;
    assert_eq!(
        unsafe { DATA_CONSTRUCTION.retire(cell, &mut cancelled, Fast) },
        Err(cell::Error::Access)
    );
    assert!(cancelled.is_none());
    assert_eq!(
        unsafe { owner.advance_mapping(mapping, Fast) },
        Ok(MappingProgress::Removed)
    );
    assert_eq!(owner.observe_mapping(mapping), Err(nest::Error::Gone));
    let first = DATA_CONSTRUCTION
        .prepare(cell, bytes, Rights::READ_WRITE, &FIRST_ACCESS, Fast)
        .unwrap();
    let complete = unsafe { owner.map(span, virtual_address, &FIRST_ACCESS, first, Fast) }.unwrap();
    assert_ne!(complete, mapping);
    assert_eq!(
        unsafe { owner.advance_mapping(complete, Fast) },
        Ok(MappingProgress::Pending)
    );
    assert_eq!(
        unsafe { owner.advance_mapping(complete, Fast) },
        Ok(MappingProgress::Active)
    );
    let mapping = complete;
    assert_eq!(load(address), 0);
    assert_eq!(load(address + PAGE), 0);
    let written = 0x1742_ace0_dad0_3210u64;
    let adjacent = 0xe84b_9913_1020_321fu64;
    store(address, written);
    store(address + PAGE, adjacent);
    owner.begin_unmap(mapping).unwrap();
    assert_eq!(
        owner.observe_mapping(mapping).unwrap().phase,
        MappingPhase::Removing
    );
    let mut retired = None;
    assert_eq!(
        unsafe { DATA_CONSTRUCTION.retire(cell, &mut retired, Fast) },
        Err(cell::Error::Access)
    );
    assert!(retired.is_none());
    assert_eq!(
        unsafe { owner.advance_mapping(mapping, Fast) },
        Ok(MappingProgress::Pending)
    );
    assert_eq!(
        owner.observe_mapping(mapping).unwrap().phase,
        MappingPhase::Removing
    );
    assert_eq!(
        unsafe { DATA_CONSTRUCTION.retire(cell, &mut retired, Fast) },
        Err(cell::Error::Access)
    );
    assert_eq!(
        unsafe { owner.advance_mapping(mapping, Fast) },
        Ok(MappingProgress::Removed)
    );
    assert_eq!(owner.observe_mapping(mapping), Err(nest::Error::Gone));

    let replacement =
        unsafe { owner.map(span, virtual_address, &SECOND_ACCESS, second, Fast) }.unwrap();
    assert_ne!(mapping, replacement);
    assert_eq!(owner.begin_unmap(mapping), Err(nest::Error::Gone));
    assert_eq!(
        unsafe { owner.advance_mapping(replacement, Fast) },
        Ok(MappingProgress::Pending)
    );
    assert_eq!(
        unsafe { owner.advance_mapping(replacement, Fast) },
        Ok(MappingProgress::Active)
    );
    assert_eq!(load(address), written);
    assert_eq!(load(address + PAGE), adjacent);
    let late = DATA_CONSTRUCTION
        .prepare(cell, bytes, Rights::READ, &FIRST_ACCESS, Fast)
        .unwrap();
    DATA_CONSTRUCTION.revoke(cell).unwrap();
    // The direct grant governs admission, not ongoing one-time accepted use.
    assert_eq!(
        owner.observe_mapping(replacement).unwrap().phase,
        MappingPhase::Active
    );
    owner.begin_release_span(span).unwrap();
    assert_eq!(owner.finish_release_span(span), Err(nest::Error::Busy));
    assert_eq!(
        unsafe { owner.map(span, other_address, &FIRST_ACCESS, late, Fast) },
        Err(nest::Error::Shut)
    );
    assert_eq!(FIRST_ACCESS.observe(), Ok(Some(late)));
    assert_eq!(
        unsafe { owner.advance_mapping(replacement, Fast) },
        Ok(MappingProgress::Pending)
    );
    assert_eq!(owner.finish_release_span(span), Err(nest::Error::Busy));
    assert_eq!(
        unsafe { owner.advance_mapping(replacement, Fast) },
        Ok(MappingProgress::Removed)
    );
    owner.finish_release_span(span).unwrap();
    let next_span = owner.reserve_span(virtual_range).unwrap();
    assert_ne!(next_span, span);
    assert_eq!(
        unsafe { owner.map(next_span, virtual_address, &FIRST_ACCESS, late, Fast) },
        Err(nest::Error::Cell(cell::Error::Pend(pend::Error::Revoked)))
    );
    assert_eq!(FIRST_ACCESS.observe(), Ok(Some(late)));
    // The rejected accept must also have released the actual leaf claim.
    owner.cancel_preparation(Fast).unwrap();
    FIRST_ACCESS.cancel(late, Fast).unwrap();
    DATA_CONSTRUCTION.shut(cell, Fast).unwrap();
    DATA_CONSTRUCTION.release(cell).unwrap();
    owner.begin_release_span(next_span).unwrap();
    owner.finish_release_span(next_span).unwrap();
    unsafe { DATA_CONSTRUCTION.retire(cell, &mut retired, Fast) }.unwrap();
    assert_eq!(DATA_CONSTRUCTION.locate(allocation), Ok(None));
    let mut receipt = None;
    unsafe { retired.take().unwrap().begin_return(&mut receipt) }
        .ok()
        .unwrap();
    let receipt = receipt.take().unwrap();
    let returned = receipt.finish(Fast).unwrap();
    assert_eq!(returned.allocation, allocation);
    assert_eq!(returned.physical, physical);
    receipt.acknowledge().unwrap();
    assert_eq!(allowance.balance(), Ok(balance));
    kernel::metal::platform::boot::treaty::write(
        "PASS ordinary-mapping allocation=1 zero=1 joint=1 conflict=1 capacity=1 rw=1 remap=1 close=1 revoke=1 finish=1 refund=1 supply=1 leaves=2 partial=1\n",
    );
}
