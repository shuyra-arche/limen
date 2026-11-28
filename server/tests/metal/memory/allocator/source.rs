use crate::{
    basis::pend::{Error as PendError, Handle, Initial as InitialPend, PendSpace, Revoker},
    metal::{
        memory::access::treaty::ram::MappedRange,
        memory::allocator::treaty::{
            Allocation, Allocator, Allowance, BudgetObservation, BudgetProgress, Config, Error,
            Initial, LookupObservation, Pending, RamPurpose::Metadata, RegionDescription,
            RegionOrigin, Request, Retired, ReturnReceipt, Search,
        },
        sync::treaty::{
            TurnAcquireError,
            TurnMode::{Fast, Slow},
        },
    },
};
use crate::{
    entry::boot::{
        Boot, Regions, Retirement,
        tests::{describe, image, organization::retired, range},
    },
    metal::platform::{boot::handoff::Descriptions, firmware::treaty::map::FirmwareMap},
};
use core::mem::MaybeUninit;
use limen_treaty::model::{Alignment, ByteLength, PhysicalRange};
use std::{boxed::Box, vec::Vec};

const PAGE: u64 = 4096;
const BASE: u64 = 0x100_000;
const ATTRIBUTES: RegionOrigin = RegionOrigin::Uefi { attributes: 8 };
const CONFIG: Config = Config {
    granule: ByteLength::new(PAGE),
    maximum: ByteLength::new(16 * PAGE),
    regions: 3,
    grains: 16,
    roots: 8,
    allocations: 4,
    returns: 6,
    allowances: 2,
    edits: 128,
};

fn pages(first: u64, last: u64) -> PhysicalRange {
    range(BASE + first * PAGE, BASE + last * PAGE)
}

#[repr(align(4096))]
struct Mapped([MaybeUninit<u8>; 2 * PAGE as usize]);

#[repr(align(8))]
struct Descriptors([u8; 40]);

pub(super) fn bootstrap(
    operation: impl FnOnce(&mut Regions<'_>, &mut Option<MappedRange<'static>>),
) {
    bootstrap_at(BASE, operation);
}

fn bootstrap_at(
    base: u64,
    operation: impl FnOnce(&mut Regions<'_>, &mut Option<MappedRange<'static>>),
) {
    let mut descriptors = Descriptors([0; 40]);
    let scope = range(base, base + 13 * PAGE);
    describe(&mut descriptors.0, 7, scope.start().get(), 13, 8);
    let map = FirmwareMap::new(&descriptors.0, 40, 1, 1).unwrap();
    let descriptions = Descriptions::new();
    let boot = Boot::new(0, image(), Some(map), &descriptions);
    let exclusions = boot.exclusions_for(image(), image());
    let evidence = retired(scope);
    let mut scratch = [None; 3];
    let mut regions = Regions::organize(
        &boot,
        &exclusions,
        ByteLength::new(PAGE),
        Some(&evidence),
        &mut scratch,
    )
    .unwrap();
    let backing = Box::leak(Box::new(Mapped([MaybeUninit::uninit(); 2 * PAGE as usize])));
    let physical = range(base + 4 * PAGE, base + 6 * PAGE);
    let mut mapping = Some(unsafe { MappedRange::new(physical, &mut backing.0) }.unwrap());
    operation(&mut regions, &mut mapping);
}

fn provision(
    config: Config,
    capacity: u64,
    initial: &mut Option<Allocation>,
    allowance: &mut Option<Allowance>,
) {
    bootstrap(|regions, mapping| {
        // Drop the transient source result. The receiver keeps the original
        // close handle, its exact source, and the separate material receiver.
        assert!(
            unsafe {
                regions.allocator(
                    mapping,
                    config,
                    capacity,
                    allowance,
                    &mut [Initial::new(pages(0, 3), initial)],
                )
            }
            .unwrap()
            .is_some()
        );
        assert!(mapping.is_none());
        assert_eq!(regions.entries().count(), 0);
    });
}

fn source(initial: &mut Option<Allocation>, capacity: u64) -> (&'static Allocator, Allowance) {
    let mut receiver = None;
    provision(CONFIG, capacity, initial, &mut receiver);
    let allowance = receiver.unwrap();
    (allowance.source(), allowance)
}

fn begin_return(material: Retired) -> ReturnReceipt {
    let mut receiver = None;
    // The fixture retains the original receipt through settlement.
    unsafe { material.begin_return(&mut receiver) }
        .ok()
        .unwrap();
    receiver.unwrap()
}

fn inspect_budget(source: &Allocator) -> Result<(ByteLength, ByteLength), Error> {
    let mut observations = [BudgetObservation::EMPTY; 3];
    let mut budget = source.budget(&mut observations)?;
    loop {
        match budget.step()? {
            BudgetProgress::Pending => {}
            BudgetProgress::Complete(managed, unassigned) => return Ok((managed, unassigned)),
        }
    }
}

fn initial_capacity(source: &Allocator) -> u64 {
    let root_pages = CONFIG.layout().unwrap().size().div_ceil(PAGE as usize) as u64;
    assert_eq!(source.root(), pages(4, 4 + root_pages));
    (13 - root_pages) * PAGE
}

#[test]
fn only_the_original_source_can_resolve_region_and_allowance_slots() {
    use crate::metal::sync::treaty::TurnWorkspace;

    let mut original = None;
    let (source, allowance) = source(&mut original, 4 * PAGE);
    let original = original.unwrap();
    let region = original.region();
    let candidate = source.allowance_candidate().unwrap();
    let mut other_allowance = None;
    let other_base = BASE + 32 * PAGE;
    bootstrap_at(other_base, |regions, mapping| {
        unsafe { regions.allocator(mapping, CONFIG, PAGE, &mut other_allowance, &mut []) }
            .unwrap()
            .unwrap();
    });
    let other_allowance = other_allowance.unwrap();
    let other = other_allowance.source();
    let other_region = other.regions().next().unwrap().0;
    let other_candidate = other.allowance_candidate().unwrap();
    assert_ne!(region, other_region);
    assert_ne!(allowance.id(), other_allowance.id());
    assert_ne!(candidate, other_candidate);

    let budget = inspect_budget(source)
        .map(|(total, unassigned)| (total.get(), unassigned.get()))
        .unwrap();
    let other_budget = inspect_budget(other)
        .map(|(total, unassigned)| (total.get(), unassigned.get()))
        .unwrap();
    let mut work = TurnWorkspace::<4>::new();
    let request = Request::new(
        ByteLength::new(PAGE),
        Alignment::new(PAGE).unwrap(),
        range(other_base, other_base + 13 * PAGE),
        None,
        Metadata,
    )
    .unwrap();
    assert!(matches!(
        other.lookup(request, Some(region), &mut [LookupObservation::EMPTY; 3]),
        Err(Error::Stale)
    ));
    assert_eq!(
        other.plan_allowance(
            &mut work.plan(),
            candidate,
            &[(other_region, ByteLength::new(PAGE))]
        ),
        Err(Error::Stale)
    );
    assert_eq!(
        other.plan_allowance(
            &mut work.plan(),
            other_candidate,
            &[(region, ByteLength::new(PAGE))]
        ),
        Err(Error::Stale)
    );
    assert_eq!(
        other.plan_transfer(&mut work.plan(), allowance.id(), other_allowance.id()),
        Err(Error::Stale)
    );
    assert_eq!(allowance.shut(Some(other_region), Fast), Err(Error::Stale));
    assert_eq!(
        inspect_budget(source).map(|(total, unassigned)| (total.get(), unassigned.get())),
        Ok(budget)
    );
    assert_eq!(
        inspect_budget(other).map(|(total, unassigned)| (total.get(), unassigned.get())),
        Ok(other_budget)
    );

    allowance.shut(Some(region), Fast).unwrap();
    let (allowance, failure) = allowance.reclaim(Fast).err().unwrap();
    assert_eq!(failure, Error::Balance);
    let receipt = begin_return(unsafe { original.retire() });
    let mut cursor = 0;
    assert_eq!(other.pending(region, &mut cursor, 1), Err(Error::Stale));
    assert_eq!(cursor, 0);
    assert_eq!(other.finish_return(receipt.id(), Fast), Err(Error::Stale));
    assert_eq!(
        source.pending(region, &mut cursor, 1),
        Ok(Pending::Found(receipt.id()))
    );
    source.finish_return(receipt.id(), Fast).unwrap();
    allowance.reclaim(Fast).ok().unwrap();
    receipt.acknowledge().unwrap();
    assert_eq!(
        inspect_budget(other).map(|(total, unassigned)| (total.get(), unassigned.get())),
        Ok(other_budget)
    );

    other_allowance.shut(Some(other_region), Fast).unwrap();
    other_allowance.reclaim(Fast).ok().unwrap();
}

fn metadata_spending(allowance: &Allowance) -> (Handle, Revoker) {
    let space = Box::leak(Box::new(PendSpace::new()));
    let mut spending = None;
    let mut closing = None;
    unsafe {
        space.publish_initial(
            allowance,
            ByteLength::new(PAGE),
            Alignment::new(PAGE).unwrap(),
            InitialPend {
                spending: &mut spending,
                closing: &mut closing,
            },
            Fast,
        )
    }
    .unwrap();
    (spending.unwrap(), closing.unwrap())
}

fn metadata_candidate(
    source: &'static Allocator,
) -> crate::metal::memory::allocator::treaty::Candidate {
    let request = Request::new(
        ByteLength::new(PAGE),
        Alignment::new(PAGE).unwrap(),
        pages(3, 4),
        Some(ATTRIBUTES),
        Metadata,
    )
    .unwrap();
    let Search::Candidate(candidate) = source
        .lookup(request, None, &mut [LookupObservation::EMPTY; 3])
        .unwrap()
        .step()
        .unwrap()
    else {
        panic!("expected the fixture's free metadata page");
    };
    candidate
}

#[test]
fn initial_pend_limits_and_revalidates_the_original_spender() {
    use crate::metal::{
        memory::allocator::treaty::{AllocationConstruction, RamPurpose},
        sync::treaty::TurnWorkspace,
    };
    use std::{sync::mpsc::sync_channel, thread};

    let cancelled = &*Box::leak(Box::new(AllocationConstruction::new()));
    let issued = Box::leak(Box::new(AllocationConstruction::new()));
    let mut initial = None;
    let (allowance, before, issued_id) = {
        let (source, allowance) = source(&mut initial, 8 * PAGE);
        let space = Box::leak(Box::new(PendSpace::new()));
        let other = Box::leak(Box::new(PendSpace::new()));
        let mut spending = None;
        let mut closing = None;
        let mut different = None;
        let mut other_closing = None;
        unsafe {
            assert_eq!(
                space.publish_initial(
                    &allowance,
                    ByteLength::new(0),
                    Alignment::new(PAGE).unwrap(),
                    InitialPend {
                        spending: &mut spending,
                        closing: &mut closing,
                    },
                    Fast
                ),
                Err(PendError::Source(Error::PermitLimit))
            );
            assert!(spending.is_none() && closing.is_none());
            space
                .publish_initial(
                    &allowance,
                    ByteLength::new(PAGE),
                    Alignment::new(PAGE).unwrap(),
                    InitialPend {
                        spending: &mut spending,
                        closing: &mut closing,
                    },
                    Fast,
                )
                .unwrap();
            assert_eq!(
                other.publish_initial(
                    &allowance,
                    ByteLength::new(PAGE),
                    Alignment::new(PAGE).unwrap(),
                    InitialPend {
                        spending: &mut different,
                        closing: &mut closing,
                    },
                    Fast
                ),
                Err(PendError::Receiver)
            );
            assert!(different.is_none());
            other
                .publish_initial(
                    &allowance,
                    ByteLength::new(PAGE),
                    Alignment::new(PAGE).unwrap(),
                    InitialPend {
                        spending: &mut different,
                        closing: &mut other_closing,
                    },
                    Fast,
                )
                .unwrap();
        }
        let handle = spending.unwrap();
        let revoker = closing.unwrap();
        let different = different.unwrap();
        let other_revoker = other_closing.unwrap();
        let candidate = |length, alignment, purpose| {
            let request = Request::new(
                ByteLength::new(length),
                Alignment::new(alignment).unwrap(),
                pages(6, 13),
                None,
                purpose,
            )
            .unwrap();
            let mut lookup_observations = [LookupObservation::EMPTY; 3];
            let mut lookup = source
                .lookup(request, None, &mut lookup_observations)
                .unwrap();
            loop {
                match lookup.step().unwrap() {
                    Search::Pending => {}
                    Search::Candidate(candidate) => break candidate,
                    Search::NoRange => panic!("expected a candidate in the fixture"),
                }
            }
        };
        let before = allowance.balance().unwrap();
        for invalid in [
            candidate(PAGE, PAGE, RamPurpose::Cell),
            candidate(2 * PAGE, PAGE, Metadata),
            candidate(PAGE, 2 * PAGE, Metadata),
        ] {
            assert_eq!(
                handle.reserve(invalid, cancelled, Fast),
                Err(PendError::Source(Error::PermitLimit))
            );
        }
        assert_eq!(allowance.balance(), Ok(before));

        let valid = candidate(PAGE, PAGE, Metadata);
        let (entered, held) = sync_channel(0);
        let (release, released) = sync_channel(0);
        thread::scope(|scope| {
            scope.spawn(move || {
                let mut work = TurnWorkspace::<1>::new();
                let mut plan = work.plan();
                cancelled.plan(&mut plan).unwrap();
                plan.acquire(Fast, |_| {
                    entered.send(()).unwrap();
                    released.recv().unwrap();
                })
                .unwrap();
            });
            held.recv().unwrap();
            let result = handle.reserve(valid, cancelled, Fast);
            release.send(()).unwrap();
            assert_eq!(result, Err(PendError::Acquire(TurnAcquireError::Busy)));
        });
        assert_eq!(allowance.balance(), Ok(before));
        handle.reserve(valid, cancelled, Fast).unwrap();
        assert_eq!(cancelled.cancel(Fast), Err(Error::Receiver));
        assert_eq!(
            different.issue(cancelled, Fast),
            Err(PendError::Source(Error::PermitOrigin))
        );
        assert_eq!(revoker.revoked(), Ok(false));
        revoker.revoke().unwrap();
        assert_eq!(handle.issue(cancelled, Fast), Err(PendError::Revoked));
        assert_eq!(allowance.balance().unwrap().reserved().get(), PAGE);
        assert_eq!(
            handle.reserve(valid, cancelled, Fast),
            Err(PendError::Revoked)
        );
        handle.release().unwrap();
        assert_eq!(handle.release(), Err(PendError::Exhausted));
        assert_eq!(revoker.revoked(), Ok(true));
        assert_eq!(revoker.revoke(), Ok(()));
        assert_eq!(
            unsafe {
                space.publish_initial(
                    &allowance,
                    ByteLength::new(PAGE),
                    Alignment::new(PAGE).unwrap(),
                    InitialPend {
                        spending: &mut None,
                        closing: &mut None,
                    },
                    Fast,
                )
            },
            Err(PendError::Exhausted)
        );
        assert_eq!(other_revoker.revoked(), Ok(false));
        let id = different
            .reserve(candidate(PAGE, PAGE, Metadata), issued, Fast)
            .unwrap();
        assert_eq!(different.issue(issued, Fast), Ok(id));
        different.release().unwrap();
        assert_eq!(other_revoker.revoked(), Ok(false));
        other_revoker.revoke().unwrap();
        assert_eq!(other_revoker.revoked(), Ok(true));
        (allowance, before, id)
    };

    // The recovery owner starts with the original receivers. The boot source
    // binding and both spending holdings are gone.
    cancelled.shut().unwrap();
    assert_eq!(cancelled.cancel(Fast), Ok(()));
    assert_eq!(cancelled.cancel(Fast), Ok(()));
    assert_eq!(allowance.balance().unwrap().reserved().get(), 0);
    let mut material = None;
    unsafe { issued.take(&mut material) }.unwrap();
    let mut receipt = None;
    let material = material.unwrap();
    assert_eq!(material.initial_access(), None);
    let retired = unsafe { material.retire() };
    unsafe { retired.begin_return(&mut receipt) }.ok().unwrap();
    let receipt = receipt.unwrap();
    assert_eq!(receipt.acknowledge(), Err(Error::Stale));
    assert_eq!(receipt.finish(Fast).unwrap().allocation, issued_id);
    assert_eq!(receipt.finish(Fast).unwrap().allocation, issued_id);
    receipt.acknowledge().unwrap();
    assert_eq!(receipt.finish(Fast), Err(Error::Stale));
    assert_eq!(allowance.balance(), Ok(before));
}

#[test]
fn table_page_spending_retains_its_origin_and_bounds() {
    use crate::metal::memory::allocator::treaty::{
        AllocationConstruction, InitialAccess, RamPurpose::PageTable,
    };

    let mut initial = None;
    let (source, allowance) = source(&mut initial, 8 * PAGE);
    assert_eq!(initial.as_ref().unwrap().initial_access(), None);
    let (metadata, _) = metadata_spending(&allowance);
    let table_spending = || {
        let space = Box::leak(Box::new(PendSpace::for_page_tables()));
        let mut spending = None;
        let mut closing = None;
        unsafe {
            space.publish_initial(
                &allowance,
                ByteLength::new(2 * PAGE),
                Alignment::new(2 * PAGE).unwrap(),
                InitialPend {
                    spending: &mut spending,
                    closing: &mut closing,
                },
                Fast,
            )
        }
        .unwrap();
        (spending.unwrap(), closing.unwrap())
    };
    let (spending, revoker) = table_spending();
    let (other, other_revoker) = table_spending();
    let candidate = |length, alignment| {
        let request = Request::new(
            ByteLength::new(length),
            Alignment::new(alignment).unwrap(),
            pages(6, 13),
            Some(ATTRIBUTES),
            PageTable,
        )
        .unwrap();
        let mut observations = [LookupObservation::EMPTY; 3];
        let mut lookup = source.lookup(request, None, &mut observations).unwrap();
        loop {
            match lookup.step().unwrap() {
                Search::Pending => {}
                Search::Candidate(candidate) => break candidate,
                Search::NoRange => panic!("expected room for table-page spending"),
            }
        }
    };

    let before = allowance.balance().unwrap();
    let receiver = Box::leak(Box::new(AllocationConstruction::new()));
    let valid = candidate(2 * PAGE, 2 * PAGE);
    assert_eq!(valid.physical().start().get() % (2 * PAGE), 0);
    assert_eq!(
        metadata.reserve(candidate(PAGE, PAGE), receiver, Fast),
        Err(PendError::Source(Error::PermitLimit))
    );
    for invalid in [
        metadata_candidate(source),
        candidate(3 * PAGE, PAGE),
        candidate(PAGE, 4 * PAGE),
    ] {
        assert_eq!(
            spending.reserve(invalid, receiver, Fast),
            Err(PendError::Source(Error::PermitLimit))
        );
    }
    assert_eq!(allowance.balance(), Ok(before));

    let id = spending.reserve(valid, receiver, Fast).unwrap();
    assert_eq!(
        other.issue(receiver, Fast),
        Err(PendError::Source(Error::PermitOrigin))
    );
    assert_eq!(allowance.balance().unwrap().reserved().get(), 2 * PAGE);
    assert_eq!(spending.issue(receiver, Fast), Ok(id));
    revoker.revoke().unwrap();
    spending.release().unwrap();
    let mut material = None;
    unsafe { receiver.take(&mut material) }.unwrap();
    let material = material.unwrap();
    assert_eq!(material.id(), id);
    assert_eq!(material.physical(), valid.physical());
    assert_eq!(material.initial_access(), Some(InitialAccess::ReadWrite));

    let receipt = begin_return(unsafe { material.retire() });
    assert_eq!(receipt.finish(Fast).unwrap().allocation, id);
    assert_eq!(receipt.finish(Fast).unwrap().physical, valid.physical());
    receipt.acknowledge().unwrap();
    assert_eq!(allowance.balance(), Ok(before));

    let receiver = Box::leak(Box::new(AllocationConstruction::new()));
    let id = other
        .reserve(candidate(2 * PAGE, 2 * PAGE), receiver, Fast)
        .unwrap();
    assert_eq!(other.issue(receiver, Fast), Ok(id));
    let mut material = None;
    unsafe { receiver.take(&mut material) }.unwrap();
    let material = material.unwrap();
    assert_eq!(material.initial_access(), Some(InitialAccess::ReadWrite));
    let middle = material
        .physical()
        .start()
        .advance(ByteLength::new(PAGE))
        .unwrap();
    let mut halves = [None, None];
    unsafe { source.split(material.retire(), middle, &mut halves, Fast) }
        .ok()
        .unwrap();
    let [left, right] = halves.map(Option::unwrap);
    assert_eq!(
        [left.initial_access(), right.initial_access()],
        [None, None]
    );

    let mut merged = None;
    unsafe { source.merge(left.retire(), right.retire(), &mut merged, Fast) }
        .ok()
        .unwrap();
    let merged = merged.unwrap();
    assert_eq!(merged.initial_access(), None);
    assert_ne!(merged.id(), id);
    other_revoker.revoke().unwrap();
    other.release().unwrap();
    let receipt = begin_return(unsafe { merged.retire() });
    receipt.finish(Fast).unwrap();
    receipt.acknowledge().unwrap();
    assert_eq!(allowance.balance(), Ok(before));
}

#[test]
fn ordinary_cell_spending_cannot_publish_a_table_page() {
    use crate::{
        basis::cell::CellSlot,
        metal::{
            memory::{
                allocator::treaty::{
                    AllocationConstruction, InitialAccess, MappedStorage, RamPurpose,
                },
                mmu::{
                    riscv64::internal::format::Format,
                    treaty::{TableError, TableFormat, TableStore},
                },
            },
            sync::treaty::TurnWorkspace,
        },
    };

    let mut original = None;
    let (source, allowance) = source(&mut original, 8 * PAGE);
    let publish = |space: &'static PendSpace| {
        let mut spending = None;
        let mut closing = None;
        unsafe {
            space.publish_initial(
                &allowance,
                ByteLength::new(PAGE),
                Alignment::new(PAGE).unwrap(),
                InitialPend {
                    spending: &mut spending,
                    closing: &mut closing,
                },
                Fast,
            )
        }
        .unwrap();
        (spending.unwrap(), closing.unwrap())
    };
    let (spending, closing) = publish(Box::leak(Box::new(PendSpace::for_cells())));
    let (table_spending, table_closing) =
        publish(Box::leak(Box::new(PendSpace::for_page_tables())));
    let candidate = |purpose| {
        let request = Request::new(
            ByteLength::new(PAGE),
            Alignment::new(PAGE).unwrap(),
            pages(6, 13),
            Some(ATTRIBUTES),
            purpose,
        )
        .unwrap();
        let mut observations = [LookupObservation::EMPTY; 3];
        let mut lookup = source.lookup(request, None, &mut observations).unwrap();
        loop {
            match lookup.step().unwrap() {
                Search::Pending => {}
                Search::Candidate(candidate) => break candidate,
                Search::NoRange => panic!("page available"),
            }
        }
    };
    let before = allowance.balance().unwrap();
    let position = Box::leak(Box::new(AllocationConstruction::new()));
    assert_eq!(
        spending.reserve(candidate(RamPurpose::PageTable), position, Fast),
        Err(PendError::Source(Error::PermitLimit))
    );
    assert_eq!(
        table_spending.reserve(candidate(RamPurpose::Cell), position, Fast),
        Err(PendError::Source(Error::PermitLimit))
    );
    assert_eq!(allowance.balance(), Ok(before));
    let id = spending
        .reserve(candidate(RamPurpose::Cell), position, Fast)
        .unwrap();
    assert_eq!(spending.issue(position, Fast), Ok(id));
    let mut material = None;
    unsafe { position.take(&mut material) }.unwrap();
    let material = material.unwrap();
    let bytes = Box::leak(Box::new(Mapped([MaybeUninit::uninit(); 2 * PAGE as usize])));
    let physical = material.physical();
    let map = unsafe { MappedRange::new(physical, &mut bytes.0[..PAGE as usize]) }.unwrap();
    let storage = unsafe { MappedStorage::prepare(material, map) }
        .ok()
        .unwrap();

    let mut work = TurnWorkspace::<2>::new();
    let mut plan = work.plan();
    spending.plan_initial_access(&mut plan).unwrap();
    assert_eq!(
        plan.acquire(Fast, |held| {
            spending.initial_access(held, &storage, RamPurpose::Cell)
        })
        .unwrap(),
        Ok(InitialAccess::ReadWrite)
    );

    let cell = Box::leak(Box::new(CellSlot::new()));
    let tables = Box::leak(Box::new(TableStore::new(TableFormat::Riscv64(
        Format::Sv39,
    ))));
    let (storage, error) = tables
        .publish_cell(cell, spending, storage, Fast)
        .err()
        .unwrap();
    assert_eq!(
        error,
        TableError::Pend(PendError::Source(Error::PermitLimit))
    );
    assert_eq!(storage.id(), id);
    assert_eq!(tables.locate(id), Ok(None));

    closing.revoke().unwrap();
    spending.release().unwrap();
    let (material, _) = storage.into_parts();
    let receipt = begin_return(unsafe { material.retire() });
    assert_eq!(receipt.finish(Fast).unwrap().allocation, id);
    receipt.acknowledge().unwrap();
    table_closing.revoke().unwrap();
    table_spending.release().unwrap();
    assert_eq!(allowance.balance(), Ok(before));
}

#[test]
fn ordinary_cell_publication_owns_cleared_material_and_an_independent_holding() {
    use crate::{
        basis::cell::{CellConstruction, CellSlot, Error as CellError},
        metal::memory::allocator::treaty::{AllocationConstruction, MappedStorage, RamPurpose},
    };
    use limen_treaty::model::{ByteOffset, ByteRange};

    let mut original = None;
    let (source, allowance) = source(&mut original, 8 * PAGE);
    let publish = || {
        let space = Box::leak(Box::new(PendSpace::for_cells()));
        let mut spending = None;
        let mut closing = None;
        unsafe {
            space.publish_initial(
                &allowance,
                ByteLength::new(PAGE),
                Alignment::new(PAGE).unwrap(),
                InitialPend {
                    spending: &mut spending,
                    closing: &mut closing,
                },
                Fast,
            )
        }
        .unwrap();
        (spending.unwrap(), closing.unwrap())
    };
    let (spending, closing) = publish();
    let (other, other_closing) = publish();
    let request = Request::new(
        ByteLength::new(PAGE),
        Alignment::new(PAGE).unwrap(),
        pages(6, 13),
        Some(ATTRIBUTES),
        RamPurpose::Cell,
    )
    .unwrap();
    let mut observations = [LookupObservation::EMPTY; 3];
    let mut lookup = source.lookup(request, None, &mut observations).unwrap();
    let candidate = loop {
        match lookup.step().unwrap() {
            Search::Pending => {}
            Search::Candidate(candidate) => break candidate,
            Search::NoRange => panic!("page available"),
        }
    };
    let before = allowance.balance().unwrap();
    let position = Box::leak(Box::new(AllocationConstruction::new()));
    let id = spending.reserve(candidate, position, Fast).unwrap();
    assert_eq!(spending.issue(position, Fast), Ok(id));
    let mut material = None;
    unsafe { position.take(&mut material) }.unwrap();
    let material = material.unwrap();
    let physical = material.physical();
    let bytes = Box::leak(Box::new(Mapped([MaybeUninit::new(0xa5); 2 * PAGE as usize])));
    let map = unsafe { MappedRange::new(physical, &mut bytes.0[..PAGE as usize]) }.unwrap();
    let material = unsafe { MappedStorage::prepare(material, map) }
        .ok()
        .unwrap();
    let cell = Box::leak(Box::new(CellSlot::new()));
    let construction = Box::leak(Box::new(CellConstruction::new()));

    let (material, error) = construction
        .publish(cell, other, material, Fast)
        .err()
        .unwrap();
    assert_eq!(
        error,
        CellError::Pend(PendError::Source(Error::PermitOrigin))
    );
    assert_eq!(material.id(), id);
    assert_eq!(construction.locate(id), Ok(None));
    let (material, mut map) = material.into_parts();
    assert!(
        map.bytes()
            .iter()
            .all(|byte| unsafe { byte.assume_init() } == 0)
    );
    let material = unsafe { MappedStorage::prepare(material, map) }
        .ok()
        .unwrap();
    let published = construction.publish(cell, spending, material, Fast).ok().unwrap();
    let range = ByteRange::new(ByteOffset::new(0), ByteOffset::new(PAGE)).unwrap();
    assert_eq!(construction.locate(id), Ok(Some(published)));
    assert_eq!(construction.initial_range(published), Ok(range));
    closing.revoke().unwrap();
    spending.release().unwrap();
    assert_eq!(construction.initial_range(published), Ok(range));
    construction.revoke(published).unwrap();
    assert_eq!(
        construction.initial_range(published),
        Err(CellError::Pend(PendError::Revoked))
    );
    construction.release(published).unwrap();
    assert_eq!(
        construction.initial_range(published),
        Err(CellError::Pend(PendError::Exhausted))
    );
    assert_eq!(construction.locate(id), Ok(Some(published)));
    assert_eq!(allowance.balance().unwrap().occupied().get(), before.occupied().get() + PAGE);
    let mut retired = None;
    unsafe { construction.retire(published, &mut retired, Fast) }.unwrap();
    assert_eq!(construction.locate(id), Ok(None));
    assert_eq!(construction.initial_range(published), Err(CellError::Scope));
    let receipt = begin_return(retired.take().unwrap());
    assert_eq!(receipt.finish(Fast).unwrap().allocation, id);
    receipt.acknowledge().unwrap();
    assert_eq!(allowance.balance(), Ok(before));
    other_closing.revoke().unwrap();
    other.release().unwrap();
}

#[test]
fn closing_the_allowance_rejects_first_cell_publication_and_retains_the_allocation() {
    use crate::{
        basis::cell::CellSlot,
        metal::memory::{
            allocator::treaty::{AllocationConstruction, MappedStorage, RamPurpose::PageTable},
            mmu::{
                riscv64::internal::format::Format,
                treaty::{TableError, TableFormat, TableStore},
            },
        },
    };

    let mut original = None;
    let (source, allowance) = source(&mut original, 8 * PAGE);
    let space = Box::leak(Box::new(PendSpace::for_page_tables()));
    let mut spending = None;
    let mut closing = None;
    unsafe {
        space.publish_initial(
            &allowance,
            ByteLength::new(PAGE),
            Alignment::new(PAGE).unwrap(),
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
        ByteLength::new(PAGE),
        Alignment::new(PAGE).unwrap(),
        pages(6, 13),
        Some(ATTRIBUTES),
        PageTable,
    )
    .unwrap();
    let mut observations = [LookupObservation::EMPTY; 3];
    let mut lookup = source.lookup(request, None, &mut observations).unwrap();
    let candidate = loop {
        match lookup.step().unwrap() {
            Search::Pending => {}
            Search::Candidate(candidate) => break candidate,
            Search::NoRange => panic!("page available"),
        }
    };
    let position = Box::leak(Box::new(AllocationConstruction::new()));
    let id = spending.reserve(candidate, position, Fast).unwrap();
    assert_eq!(spending.issue(position, Fast), Ok(id));
    let mut material = None;
    unsafe { position.take(&mut material) }.unwrap();
    let material = material.unwrap();
    allowance.shut(Some(material.region()), Fast).unwrap();

    let bytes = Box::leak(Box::new(Mapped([MaybeUninit::uninit(); 2 * PAGE as usize])));
    let physical = material.physical();
    let map = unsafe { MappedRange::new(physical, &mut bytes.0[..PAGE as usize]) }.unwrap();
    let storage = unsafe { MappedStorage::prepare(material, map) }
        .ok()
        .unwrap();
    let cell = Box::leak(Box::new(CellSlot::new()));
    let tables = Box::leak(Box::new(TableStore::new(TableFormat::Riscv64(
        Format::Sv39,
    ))));
    let (storage, error) = tables
        .publish_cell(cell, spending, storage, Fast)
        .err()
        .unwrap();
    assert_eq!(error, TableError::Pend(PendError::Source(Error::Shut)));
    assert_eq!(storage.id(), id);
    let (material, _) = storage.into_parts();
    let receipt = begin_return(unsafe { material.retire() });
    assert_eq!(receipt.finish(Fast).unwrap().allocation, id);
    receipt.acknowledge().unwrap();
}

#[test]
fn reserve_and_revocation_order_the_original_holding() {
    use crate::metal::memory::allocator::treaty::AllocationConstruction;
    use std::{sync::Barrier, thread};

    let mut initial = None;
    let (source, allowance) = source(&mut initial, 8 * PAGE);
    let (spending, revoker) = metadata_spending(&allowance);
    let candidate = metadata_candidate(source);
    let construction = &*Box::leak(Box::new(AllocationConstruction::new()));
    let before = allowance.balance().unwrap();
    let start = Barrier::new(2);
    let reserved = thread::scope(|scope| {
        let closing = scope.spawn(|| {
            start.wait();
            revoker.revoke()
        });
        start.wait();
        let reserved = spending.reserve(candidate, construction, Slow);
        assert_eq!(closing.join().unwrap(), Ok(()));
        reserved
    });
    assert_eq!(revoker.revoked(), Ok(true));
    match reserved {
        Ok(_) => {
            let balance = allowance.balance().unwrap();
            assert_eq!(balance.reserved().get(), before.reserved().get() + PAGE);
            assert_eq!(balance.occupied().get(), before.occupied().get());
            assert_eq!(spending.issue(construction, Slow), Err(PendError::Revoked));
        }
        Err(PendError::Revoked) => assert_eq!(allowance.balance(), Ok(before)),
        unexpected => panic!("unsettled reserve/revoke race: {unexpected:?}"),
    }
    construction.shut().unwrap();
    assert_eq!(construction.cancel(Slow), Ok(()));
    assert_eq!(construction.cancel(Slow), Ok(()));
    assert_eq!(allowance.balance(), Ok(before));
    assert_eq!(metadata_candidate(source).physical(), candidate.physical());
}

#[test]
fn issue_and_allowance_shut_order_the_same_charge() {
    use crate::metal::memory::allocator::treaty::AllocationConstruction;
    use std::{sync::Barrier, thread};

    let mut initial = None;
    let (source, allowance) = source(&mut initial, 8 * PAGE);
    let (spending, _) = metadata_spending(&allowance);
    let candidate = metadata_candidate(source);
    let construction = &*Box::leak(Box::new(AllocationConstruction::new()));
    let before = allowance.balance().unwrap();
    let reserved = spending.reserve(candidate, construction, Slow).unwrap();
    let start = Barrier::new(2);
    let issued = thread::scope(|scope| {
        let closing = scope.spawn(|| {
            start.wait();
            allowance.shut(Some(candidate.region()), Slow)
        });
        start.wait();
        let issued = spending.issue(construction, Slow);
        assert_eq!(closing.join().unwrap(), Ok(()));
        issued
    });

    match issued {
        Ok(id) => {
            assert_eq!(id, reserved);
            let mut material = None;
            unsafe { construction.take(&mut material) }.unwrap();
            let receipt = begin_return(unsafe { material.unwrap().retire() });
            assert_eq!(receipt.finish(Slow).unwrap().allocation, id);
        }
        Err(PendError::Source(Error::Shut)) => {
            construction.shut().unwrap();
            construction.cancel(Slow).unwrap();
        }
        unexpected => panic!("unsettled issue/shut race: {unexpected:?}"),
    }
    let balance = allowance.balance().unwrap();
    assert_eq!(balance.reserved().get(), 0);
    assert_eq!(balance.occupied().get(), before.occupied().get());
    assert_eq!(balance.available().get(), 0);
    let capacity = initial_capacity(source);
    assert_eq!(
        inspect_budget(source).map(|(total, unassigned)| (total.get(), unassigned.get())),
        Ok((capacity, capacity - before.occupied().get()))
    );
}

#[test]
fn issue_and_cancellation_have_one_winner() {
    use crate::metal::memory::allocator::treaty::AllocationConstruction;
    use std::{sync::Barrier, thread};

    let mut initial = None;
    let (source, allowance) = source(&mut initial, 8 * PAGE);
    let (spending, _) = metadata_spending(&allowance);
    let construction = &*Box::leak(Box::new(AllocationConstruction::new()));
    let before = allowance.balance().unwrap();
    let reserved = spending
        .reserve(metadata_candidate(source), construction, Slow)
        .unwrap();
    let start = Barrier::new(2);
    let (issued, cancelled) = thread::scope(|scope| {
        let cancellation = scope.spawn(|| {
            start.wait();
            construction.shut()?;
            construction.cancel(Slow)
        });
        start.wait();
        let issued = spending.issue(construction, Slow);
        (issued, cancellation.join().unwrap())
    });

    match (issued, cancelled) {
        (Ok(id), Err(Error::Receiver)) => {
            assert_eq!(id, reserved);
            let mut material = None;
            unsafe { construction.take(&mut material) }.unwrap();
            let receipt = begin_return(unsafe { material.unwrap().retire() });
            assert_eq!(receipt.finish(Slow).unwrap().allocation, id);
        }
        (Err(PendError::Source(Error::Receiver)), Ok(())) => {
            assert_eq!(construction.cancel(Slow), Ok(()));
        }
        unexpected => panic!("unsettled issue/cancel race: {unexpected:?}"),
    }
    assert_eq!(allowance.balance(), Ok(before));
}

#[test]
fn rejects_invalid_geometry_and_receivers_before_consuming_the_root() {
    bootstrap(|regions, mapping| {
        let mut initial = None;
        let mut allowance = None;
        let input = &mut [Initial::new(pages(3, 5), &mut initial)];
        assert_eq!(
            unsafe { regions.allocator(mapping, CONFIG, 3 * PAGE, &mut allowance, input) }.err(),
            Some(Error::Range)
        );
        assert!(mapping.is_some() && initial.is_none() && allowance.is_none());
        assert_eq!(regions.entries().next().unwrap().physical, pages(0, 13));
        let config = Config {
            regions: 1,
            ..CONFIG
        };
        assert_eq!(
            unsafe { regions.allocator(mapping, config, 0, &mut allowance, &mut []) }.err(),
            Some(Error::Capacity)
        );
        assert!(mapping.is_some());
        assert!(
            unsafe { regions.allocator(&mut None, CONFIG, 0, &mut allowance, &mut []) }
                .unwrap()
                .is_none()
        );
    });
}

#[test]
fn independent_history_survives_new_generations_and_refunds_after_shut() {
    let mut initial = None;
    let (source, allowance) = source(&mut initial, 10 * PAGE);
    let capacity = initial_capacity(source);
    assert_eq!(
        inspect_budget(source).map(|(total, unassigned)| (total.get(), unassigned.get())),
        Ok((capacity, capacity - 10 * PAGE))
    );
    let mut halves = [None, None];
    let original = initial.take().unwrap();
    let original_id = original.id();
    let region = original.region();
    let [_, (credit, _)] = source
        .regions()
        .collect::<Vec<_>>()
        .try_into()
        .ok()
        .unwrap();
    let split = original
        .physical()
        .end()
        .retreat(ByteLength::new(PAGE))
        .unwrap();
    unsafe { source.split(original.retire(), split, &mut halves, Fast) }
        .ok()
        .unwrap();
    let [left, right] = halves.map(Option::unwrap);
    assert_ne!(left.id(), original_id);
    let mut merged = None;
    unsafe { source.merge(left.retire(), right.retire(), &mut merged, Slow) }
        .ok()
        .unwrap();
    let merged = merged.unwrap();
    assert_eq!(merged.physical(), pages(0, 3));
    assert_eq!(allowance.balance().unwrap().occupied().get(), 3 * PAGE);

    let receipt = begin_return(unsafe { merged.retire() });
    let mut cursor = 0;
    loop {
        match source.pending(region, &mut cursor, 1).unwrap() {
            Pending::Found(id) => {
                assert_eq!(id, receipt.id());
                break;
            }
            Pending::More => {}
            Pending::End => panic!("lost return"),
        }
    }
    allowance.shut(Some(credit), Fast).unwrap();
    let done = source.finish_return(receipt.id(), Slow).unwrap();
    assert_eq!(done.physical, pages(0, 3));
    assert_eq!(
        inspect_budget(source).map(|(total, unassigned)| (total.get(), unassigned.get())),
        Ok((capacity, capacity))
    );
    allowance.reclaim(Slow).ok().unwrap();
    assert_eq!(source.finish_return(receipt.id(), Fast), Ok(done));
    receipt.acknowledge().unwrap();
    assert_eq!(source.finish_return(receipt.id(), Fast), Err(Error::Stale));
}

#[test]
fn a_split_without_two_return_slots_keeps_the_original_material() {
    let mut initial = None;
    let mut allowance = None;
    provision(
        Config {
            allocations: 3,
            returns: 3,
            ..CONFIG
        },
        3 * PAGE,
        &mut initial,
        &mut allowance,
    );
    let allowance = allowance.unwrap();
    let source = allowance.source();
    let original = unsafe { initial.unwrap().retire() };
    let mut receivers = [None, None];
    unsafe { source.split(original, pages(0, 2).end(), &mut receivers, Fast) }
        .ok()
        .unwrap();
    let [material, returned] = receivers.map(Option::unwrap);
    let receipt = begin_return(unsafe { returned.retire() });
    let result = source.finish_return(receipt.id(), Fast).unwrap();
    let before = allowance.balance().unwrap();
    assert_eq!(before.occupied().get(), 2 * PAGE);

    let original = unsafe { material.retire() };
    let mut receivers = [None, None];
    let (original, failure) =
        unsafe { source.split(original, pages(0, 1).end(), &mut receivers, Fast) }
            .err()
            .unwrap();
    assert_eq!(failure, Error::ReturnCapacity);
    assert_eq!(original.physical(), pages(0, 2));
    assert!(receivers.iter().all(Option::is_none));
    assert_eq!(allowance.balance(), Ok(before));
    assert_eq!(source.finish_return(receipt.id(), Fast), Ok(result));

    receipt.acknowledge().unwrap();
    unsafe { source.split(original, pages(0, 1).end(), &mut receivers, Fast) }
        .ok()
        .unwrap();
    let [left, right] = receivers.map(Option::unwrap);
    assert_eq!(
        [left.physical(), right.physical()],
        [pages(0, 1), pages(1, 2)]
    );
    allowance.shut(Some(left.region()), Fast).unwrap();
    for material in [left, right] {
        let receipt = begin_return(unsafe { material.retire() });
        source.finish_return(receipt.id(), Fast).unwrap();
        receipt.acknowledge().unwrap();
    }
    allowance.reclaim(Fast).ok().unwrap();
}

#[test]
fn negative_search_checks_both_passes_and_rejects_new_geometry() {
    let mut initial = None;
    let (source, allowance) = source(&mut initial, 3 * PAGE);
    let left = initial.as_ref().unwrap().region();
    let request = Request::new(
        ByteLength::new(2 * PAGE),
        Alignment::new(PAGE).unwrap(),
        pages(0, 4),
        Some(ATTRIBUTES),
        Metadata,
    )
    .unwrap();
    let mut lookup_observations = [LookupObservation::EMPTY; 3];
    let mut lookup = source
        .lookup(request, None, &mut lookup_observations)
        .unwrap();
    loop {
        match lookup.step().unwrap() {
            Search::Pending => {}
            Search::NoRange => break,
            Search::Candidate(_) => panic!("occupied or too short"),
        }
    }

    let mut lookup_observations = [LookupObservation::EMPTY; 3];
    let mut lookup = source
        .lookup(request, Some(left), &mut lookup_observations)
        .unwrap();
    // Observe the negative left Region, then make a qualifying range free.
    assert!(matches!(lookup.step(), Ok(Search::Pending)));
    let material = initial.take().unwrap();
    let receipt = begin_return(unsafe { material.retire() });
    receipt.finish(Slow).unwrap();
    loop {
        match lookup.step() {
            Ok(Search::Pending) => {}
            Err(Error::Changed) => break,
            _ => panic!("returned a stale negative"),
        }
    }
    let mut positive_observations = [LookupObservation::EMPTY; 3];
    let mut positive = source
        .lookup(request, None, &mut positive_observations)
        .unwrap();
    let Search::Candidate(candidate) = positive.step().unwrap() else {
        panic!("the stable freed range was not found");
    };
    assert!(candidate.physical().end() <= pages(0, 4).end());
    assert_eq!(candidate.region(), left);

    let later = pages(16, 20);
    let mut adding_observations = [LookupObservation::EMPTY; 3];
    let mut adding = source
        .lookup(
            Request::new(
                ByteLength::new(PAGE),
                Alignment::new(PAGE).unwrap(),
                later,
                None,
                Metadata,
            )
            .unwrap(),
            None,
            &mut adding_observations,
        )
        .unwrap();
    let description = RegionDescription {
        physical: later,
        origin: ATTRIBUTES,
    };
    let budget_before_grant = inspect_budget(source)
        .map(|(total, unassigned)| (total.get(), unassigned.get()))
        .unwrap();
    assert_eq!(
        unsafe { Retirement::new(later).grant(source, description) }.err(),
        Some(Error::Range)
    );
    assert_eq!(
        unsafe { retired(pages(16, 19)).grant(source, description) }.err(),
        Some(Error::Range)
    );
    assert_eq!(
        inspect_budget(source).map(|(total, unassigned)| (total.get(), unassigned.get())),
        Ok(budget_before_grant)
    );
    let mut grant = unsafe { retired(later).grant(source, description) }.unwrap();
    let region = grant.admit(Fast).unwrap();
    assert_eq!(grant.admit(Slow), Ok(region));
    loop {
        match adding.step() {
            Ok(Search::Pending) => {}
            Err(Error::Changed) => break,
            _ => panic!("missed a new published Region"),
        }
    }
    let capacity = initial_capacity(source) + 4 * PAGE;
    assert_eq!(
        inspect_budget(source).map(|(total, unassigned)| (total.get(), unassigned.get())),
        Ok((capacity, capacity - 3 * PAGE))
    );
    assert_eq!(allowance.balance().unwrap().occupied().get(), 0);
}

#[test]
fn reservation_and_cancellation_use_the_complete_plan_under_contention() {
    use crate::metal::{
        memory::allocator::treaty::AllocationConstruction, sync::treaty::TurnWorkspace,
    };
    use std::{sync::mpsc::sync_channel, thread};

    // The Host owner explicitly funds its single actual construction, with a
    // revocable grant limited to its exact allowance, layout and purpose.
    let mut initial = None;
    let (source, allowance) = source(&mut initial, 8 * PAGE);
    let id = allowance.id();
    let (spending, revoker) = metadata_spending(&allowance);
    let construction = Box::leak(Box::new(AllocationConstruction::new()));
    let construction = &*construction;
    let request = Request::new(
        ByteLength::new(PAGE),
        Alignment::new(PAGE).unwrap(),
        pages(3, 4),
        Some(ATTRIBUTES),
        Metadata,
    )
    .unwrap();
    let mut lookup_observations = [LookupObservation::EMPTY; 3];
    let mut lookup = source
        .lookup(request, None, &mut lookup_observations)
        .unwrap();
    let Search::Candidate(candidate) = lookup.step().unwrap() else {
        panic!("free granule")
    };

    let (hold, held) = sync_channel(0);
    let (release, released) = sync_channel(0);
    thread::scope(|scope| {
        scope.spawn(move || {
            let mut work = TurnWorkspace::<1>::new();
            let mut plan = work.plan();
            construction.plan(&mut plan).unwrap();
            plan.acquire(Fast, |_| {
                hold.send(()).unwrap();
                released.recv().unwrap();
            })
            .unwrap();
        });
        held.recv().unwrap();
        let result = spending.reserve(candidate, construction, Fast);
        release.send(()).unwrap();
        assert_eq!(result, Err(PendError::Acquire(TurnAcquireError::Busy)));
        assert_eq!(allowance.balance().unwrap().reserved().get(), 0);
    });

    assert_eq!(candidate.physical(), pages(3, 4));
    spending.reserve(candidate, construction, Slow).unwrap();
    assert_eq!(allowance.balance().unwrap().reserved().get(), PAGE);

    let mut work = TurnWorkspace::<3>::new();
    let mut plan = work.plan();
    source
        .plan_construction(&mut plan, candidate.region(), id, construction)
        .unwrap();
    plan.acquire(Slow, |locks| construction.shut_held(locks))
        .unwrap()
        .unwrap();
    revoker.revoke().unwrap();
    allowance.shut(Some(candidate.region()), Fast).unwrap();
    construction.cancel(Slow).unwrap();
    assert_eq!(allowance.balance().unwrap().reserved().get(), 0);
    let capacity = initial_capacity(source);
    assert_eq!(
        inspect_budget(source).map(|(total, unassigned)| (total.get(), unassigned.get())),
        Ok((capacity, capacity - 3 * PAGE))
    );
}

#[test]
fn initial_allowance_can_shut_and_reclaim_without_the_boot_result() {
    use crate::metal::{
        memory::allocator::treaty::AllocationConstruction, sync::treaty::TurnWorkspace,
    };
    let mut initial = None;
    let (source, allowance) = source(&mut initial, 3 * PAGE);
    let capacity = initial_capacity(source);
    let original = allowance.id();
    let (original_spending, _) = metadata_spending(&allowance);
    let region = initial.as_ref().unwrap().region();
    let (_, credit) = source.regions().nth(1).unwrap();
    assert_eq!(credit.physical.start(), source.root().end());
    assert_eq!(credit.physical.end(), pages(0, 13).end());
    let other = source.regions().nth(1).unwrap().0;
    let debits = [
        (region, ByteLength::new(PAGE)),
        (other, ByteLength::new(2 * PAGE)),
    ];

    let mut receiver = None;
    let index = source.allowance_candidate().unwrap();
    let mut work = TurnWorkspace::<5>::new();
    let mut plan = work.plan();
    source.plan_allowance(&mut plan, index, &debits).unwrap();
    plan.acquire(Slow, |held| {
        // This fixture is the exclusive source assignment owner and keeps
        // each close responsibility in its exact receiving position.
        unsafe { source.open_allowance(held, index, &debits, &mut receiver) }
    })
    .unwrap()
    .unwrap();
    let second = receiver.take().unwrap();
    assert_eq!(second.balance().unwrap().capacity().get(), 3 * PAGE);
    assert_eq!(source.allowance_candidate(), Err(Error::AllowanceCapacity));
    assert_eq!(
        inspect_budget(source).map(|(total, unassigned)| (total.get(), unassigned.get())),
        Ok((capacity, capacity - 6 * PAGE))
    );

    allowance.shut(None, Slow).unwrap();
    let material = initial.take().unwrap();
    let receipt = begin_return(unsafe { material.retire() });
    source.finish_return(receipt.id(), Fast).unwrap();
    allowance.reclaim(Slow).ok().unwrap();
    let index = source.allowance_candidate().unwrap();
    let mut plan = work.plan();
    source.plan_allowance(&mut plan, index, &debits).unwrap();
    plan.acquire(Fast, |held| unsafe {
        source.open_allowance(held, index, &debits, &mut receiver)
    })
    .unwrap()
    .unwrap();
    let next = receiver.take().unwrap();
    assert_ne!(next.id(), original);

    let construction = &*Box::leak(Box::new(AllocationConstruction::new()));
    let candidate = metadata_candidate(source);
    assert_eq!(
        original_spending.reserve(candidate, construction, Fast),
        Err(PendError::Source(Error::Stale))
    );
    let (spending, _) = metadata_spending(&next);
    spending.reserve(candidate, construction, Fast).unwrap();
    construction.shut().unwrap();
    construction.cancel(Fast).unwrap();

    let mut plan = work.plan();
    source
        .plan_transfer(&mut plan, original, second.id())
        .unwrap();
    assert_eq!(
        plan.acquire(Fast, |held| {
            // A stale role must fail the live Allowance check before either debit.
            unsafe { source.transfer_allowance(held, original, second.id(), ByteLength::new(PAGE)) }
        }),
        Ok(Err(Error::Stale))
    );
    assert_eq!(second.balance().unwrap().capacity().get(), 3 * PAGE);
    assert_eq!(next.balance().unwrap().capacity().get(), 3 * PAGE);
    assert_eq!(
        source.finish_return(receipt.id(), Fast).unwrap().physical,
        pages(0, 3)
    );
}

#[test]
fn lost_return_response_replays_after_active_slot_reuse_and_history_saturation() {
    use crate::metal::memory::allocator::treaty::AllocationConstruction;

    let mut initial = None;
    let mut receiver = None;
    // A single active slot forces the next allocation to reuse it.
    provision(
        Config {
            allocations: 1,
            ..CONFIG
        },
        8 * PAGE,
        &mut initial,
        &mut receiver,
    );
    let allowance = receiver.unwrap();
    let source = allowance.source();
    let (spending, _) = metadata_spending(&allowance);
    let original = initial.take().unwrap();
    let original_id = original.id();
    let region = original.region();
    let mut receipts: [Option<ReturnReceipt>; CONFIG.returns] = [const { None }; CONFIG.returns];
    // The caller loses the response. Recovery discovers the Pending record
    // independently and does not read the original owner's receipt array.
    let _lost = unsafe { original.retire().begin_return(&mut receipts[0]) };
    let Pending::Found(pending) = source.pending(region, &mut 0, CONFIG.returns).unwrap() else {
        panic!("lost response made the return undiscoverable");
    };
    let original_return = source.finish_return(pending, Fast).unwrap();
    assert_eq!(original_return.allocation, original_id);
    let owner_receipt = receipts[0].take().unwrap();
    assert_eq!(owner_receipt.id(), pending);
    let request = Request::new(
        ByteLength::new(PAGE),
        Alignment::new(PAGE).unwrap(),
        pages(0, 1),
        None,
        Metadata,
    )
    .unwrap();

    for receipt in &mut receipts[1..] {
        let Search::Candidate(candidate) = source
            .lookup(request, None, &mut [LookupObservation::EMPTY; 3])
            .unwrap()
            .step()
            .unwrap()
        else {
            panic!("exact grain was not refunded");
        };
        let construction = &*Box::leak(Box::new(AllocationConstruction::new()));
        let reserved = spending.reserve(candidate, construction, Slow).unwrap();
        assert_eq!(spending.issue(construction, Slow), Ok(reserved));
        let mut issued = None;
        unsafe { construction.take(&mut issued) }.unwrap();
        let issued = issued.unwrap();
        assert_ne!(issued.id(), original_id);
        assert_eq!(issued.physical(), pages(0, 1));
        assert_eq!(
            source
                .finish_return(owner_receipt.id(), Fast)
                .unwrap()
                .allocation,
            original_id
        );
        assert_eq!(allowance.balance().unwrap().occupied().get(), PAGE);
        unsafe { issued.retire().begin_return(receipt) }
            .ok()
            .unwrap();
        receipt.as_ref().unwrap().finish(Slow).unwrap();
    }

    let Search::Candidate(candidate) = source
        .lookup(request, None, &mut [LookupObservation::EMPTY; 3])
        .unwrap()
        .step()
        .unwrap()
    else {
        panic!("metadata saturation cannot rule out a physical range");
    };
    let construction = &*Box::leak(Box::new(AllocationConstruction::new()));
    assert_eq!(
        spending.reserve(candidate, construction, Fast),
        Err(PendError::Source(Error::ReturnCapacity))
    );
    assert_eq!(allowance.balance().unwrap().available().get(), 8 * PAGE);

    owner_receipt.acknowledge().unwrap();
    let reserved = spending.reserve(candidate, construction, Slow).unwrap();
    assert_eq!(spending.issue(construction, Slow), Ok(reserved));
    assert_eq!(
        source.finish_return(owner_receipt.id(), Fast),
        Err(Error::Stale)
    );
    let mut issued = None;
    unsafe { construction.take(&mut issued) }.unwrap();
    let issued = issued.unwrap();
    let old_return = owner_receipt.id();
    let mut occupied_receiver = Some(owner_receipt);
    let (retired, error) = unsafe { issued.retire().begin_return(&mut occupied_receiver) }
        .err()
        .unwrap();
    assert_eq!(error, Error::Receiver);
    assert_eq!(occupied_receiver.as_ref().unwrap().id(), old_return);
    assert_eq!(
        source.pending(region, &mut 0, CONFIG.returns),
        Ok(Pending::End)
    );
    let new_receipt = begin_return(retired);
    source.finish_return(new_receipt.id(), Fast).unwrap();
    assert_eq!(allowance.balance().unwrap().available().get(), 8 * PAGE);
}

#[test]
fn skip_only_advances_the_lookup_with_an_outstanding_candidate() {
    let mut initial = None;
    let (source, _allowance) = source(&mut initial, 3 * PAGE);
    let request = Request::new(
        ByteLength::new(PAGE),
        Alignment::new(PAGE).unwrap(),
        pages(3, 4),
        None,
        Metadata,
    )
    .unwrap();
    let mut first_observations = [LookupObservation::EMPTY; 3];
    let mut first = source
        .lookup(request, None, &mut first_observations)
        .unwrap();
    let mut second_observations = [LookupObservation::EMPTY; 3];
    let mut second = source
        .lookup(request, None, &mut second_observations)
        .unwrap();
    let Search::Candidate(observed) = first.step().unwrap() else {
        panic!("the first lookup has an eligible grain");
    };
    assert_eq!(second.skip_current(), Err(Error::Stale));
    let Search::Candidate(replayed) = first.step().unwrap() else {
        panic!("the first lookup still holds its candidate");
    };
    assert_eq!(replayed.physical(), observed.physical());
    first.skip_current().unwrap();
    assert_eq!(first.skip_current(), Err(Error::Stale));
    assert!(matches!(first.step(), Ok(Search::Pending)));
    assert!(matches!(first.step(), Err(Error::Busy)));
    let Search::Candidate(independent) = second.step().unwrap() else {
        panic!("the second lookup did not advance");
    };
    assert_eq!(independent.physical(), observed.physical());
}

#[test]
fn source_directory_scale_is_independent_of_joint_lock_capacity() {
    use crate::metal::sync::treaty::{TurnWorkspace, turn_capacity};

    const REGIONS: usize = 6;
    assert!(turn_capacity().is_some_and(|capacity| REGIONS > capacity));
    let mut receiver = None;
    bootstrap(|regions, mapping| {
        let config = Config {
            regions: REGIONS,
            maximum: ByteLength::new(32 * PAGE),
            allocations: 1,
            returns: 1,
            ..CONFIG
        };
        unsafe { regions.allocator(mapping, config, 0, &mut receiver, &mut []) }
            .unwrap()
            .unwrap();
    });
    let allowance = receiver.take().unwrap();
    let source = allowance.source();
    let initial_count = source.regions().count();
    assert_eq!(initial_count, 2);

    for index in initial_count..REGIONS {
        let physical = pages(16 + index as u64, 17 + index as u64);
        let description = RegionDescription {
            physical,
            origin: ATTRIBUTES,
        };
        let mut grant = unsafe { retired(physical).grant(source, description) }.unwrap();
        grant.admit(Fast).unwrap();
    }
    let regions = source.regions().collect::<Vec<_>>();
    assert_eq!(regions.len(), REGIONS);

    let request = Request::new(
        ByteLength::new(PAGE),
        Alignment::new(PAGE).unwrap(),
        pages(30, 31),
        None,
        Metadata,
    )
    .unwrap();
    assert!(matches!(
        source.lookup(request, None, &mut [LookupObservation::EMPTY; REGIONS - 1]),
        Err(Error::Capacity)
    ));
    assert!(matches!(
        source.budget(&mut [BudgetObservation::EMPTY; REGIONS - 1]),
        Err(Error::Capacity)
    ));
    let mut lookup_observations = [LookupObservation::EMPTY; REGIONS];
    let mut lookup = source
        .lookup(request, None, &mut lookup_observations)
        .unwrap();
    loop {
        match lookup.step().unwrap() {
            Search::Pending => {}
            Search::NoRange => break,
            Search::Candidate(_) => panic!("unmanaged range was exposed"),
        }
    }

    let from = regions[2].0;
    let to = regions[0].0;
    let move_budget = |from, to, amount| {
        let mut work = TurnWorkspace::<2>::new();
        let mut plan = work.plan();
        source.plan_budget_transfer(&mut plan, from, to).unwrap();
        plan.acquire(Fast, |held| unsafe {
            source.transfer_budget(held, from, to, amount)
        })
        .unwrap()
        .unwrap();
    };

    // Even moving credit away and back invalidates the first observation.
    let mut observations = [BudgetObservation::EMPTY; REGIONS];
    let mut budget = source.budget(&mut observations).unwrap();
    for _ in 0..REGIONS {
        assert_eq!(budget.step(), Ok(BudgetProgress::Pending));
    }
    move_budget(from, to, ByteLength::new(PAGE));
    move_budget(to, from, ByteLength::new(PAGE));
    loop {
        match budget.step() {
            Ok(BudgetProgress::Pending) => {}
            Err(Error::Changed) => break,
            _ => panic!("a budget ABA survived the second pass"),
        }
    }

    let total = regions.iter().fold(0, |sum, (_, description)| {
        sum + description.physical.length().get()
    });
    for &(id, description) in &regions[1..] {
        move_budget(id, to, description.physical.length());
    }

    // The source still has all its credit; only placement has changed.
    let mut observations = [BudgetObservation::EMPTY; REGIONS];
    let mut budget = source.budget(&mut observations).unwrap();
    loop {
        match budget.step().unwrap() {
            BudgetProgress::Pending => {}
            BudgetProgress::Complete(managed, free) => {
                assert_eq!(managed.get(), total);
                assert_eq!(free, managed);
                break;
            }
        }
    }

    // Assign credit drawn from every Region with just three simultaneous
    // locks, not one for every Region in the source.
    let candidate = source.allowance_candidate().unwrap();
    let debits = [(to, ByteLength::new(total))];
    let mut work = TurnWorkspace::<3>::new();
    let mut plan = work.plan();
    source
        .plan_allowance(&mut plan, candidate, &debits)
        .unwrap();
    plan.acquire(Fast, |held| unsafe {
        source.open_allowance(held, candidate, &debits, &mut receiver)
    })
    .unwrap()
    .unwrap();
    let second = receiver.take().unwrap();
    assert_eq!(second.balance().unwrap().capacity().get(), total);

    let mut observations = [BudgetObservation::EMPTY; REGIONS];
    let mut budget = source.budget(&mut observations).unwrap();
    loop {
        match budget.step().unwrap() {
            BudgetProgress::Pending => {}
            BudgetProgress::Complete(managed, free) => {
                assert_eq!(managed.get(), total);
                assert_eq!(free.get(), 0);
                break;
            }
        }
    }
}

#[test]
fn region_address_index_preserves_identity_and_balances_real_admissions() {
    use crate::metal::memory::allocator::directory::tests::verify;

    // All permutations of four delayed Regions, after two initial Regions.
    // Each fixture has its own exclusive root and disjoint physical scope.
    let mut case = 0u64;
    for a in 0..4 {
        for b in 0..4 {
            for c in 0..4 {
                for d in 0..4 {
                    if a == b || a == c || a == d || b == c || b == d || c == d {
                        continue;
                    }
                    case += 1;
                    let base = BASE + case * 0x100000;
                    let mut receiver = None;
                    bootstrap_at(base, |regions, mapping| {
                        let config = Config {
                            regions: 6,
                            maximum: ByteLength::new(32 * PAGE),
                            allocations: 1,
                            returns: 1,
                            ..CONFIG
                        };
                        unsafe { regions.allocator(mapping, config, 0, &mut receiver, &mut []) }
                            .unwrap()
                            .unwrap();
                    });
                    let allowance = receiver.take().unwrap();
                    let source = allowance.source();
                    let original = source.regions().collect::<Vec<_>>();
                    assert_eq!(original.len(), 2);
                    for offset in [a, b, c, d] {
                        let physical =
                            range(base + (20 + offset) * PAGE, base + (21 + offset) * PAGE);
                        let description = RegionDescription {
                            physical,
                            origin: ATTRIBUTES,
                        };
                        let mut grant =
                            unsafe { retired(physical).grant(source, description) }.unwrap();
                        let id = grant.admit(Fast).unwrap();
                        assert_eq!(grant.admit(Slow), Ok(id));
                        assert_eq!(source.region(id).unwrap().description().physical, physical);
                        let count = source.regions().count();
                        source
                            .directory
                            .acquire(|directory| verify(directory, &source.regions[..count]))
                            .unwrap();
                    }
                    assert_eq!(source.regions().count(), 6);
                    for (id, description) in original {
                        assert_eq!(source.region(id).unwrap().description(), description);
                    }
                    let existing = source.regions().nth(3).unwrap().1.physical;
                    let count = source.regions().count();
                    source
                        .directory
                        .acquire(|directory| {
                            assert!(matches!(
                                directory.prepare(
                                    &source.regions[..count],
                                    existing,
                                    source.config.maximum
                                ),
                                Err(Error::Overlap)
                            ));
                            let overlapping =
                                range(existing.start().get(), existing.end().get() + PAGE);
                            assert!(matches!(
                                directory.prepare(
                                    &source.regions[..count],
                                    overlapping,
                                    source.config.maximum
                                ),
                                Err(Error::Overlap)
                            ));
                            let outside = range(base + 30 * PAGE, base + 31 * PAGE);
                            assert!(matches!(
                                directory.prepare(
                                    &source.regions[..count],
                                    outside,
                                    ByteLength::new(0)
                                ),
                                Err(Error::Capacity)
                            ));
                            verify(directory, &source.regions[..count]);
                        })
                        .unwrap();
                }
            }
        }
    }
    assert_eq!(case, 24);
}
