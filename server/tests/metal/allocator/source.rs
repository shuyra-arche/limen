use crate::{
    basis::pend::{Error as PendError, Handle, Initial as InitialPend, PendSpace, Revoker},
    metal::{
        allocator::{
            Allocation, Allocator, Allowance, Config, Error, Initial, Pending,
            RamPurpose::Metadata, RegionDescription, RegionOrigin, Request, Retired, ReturnId,
            Search,
        },
        mmu::internal::access::MappedRange,
        sync::{
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
    metal::{
        address::PhysicalRange,
        firmware::{facts::Descriptions, map::FirmwareMap},
    },
};
use core::mem::MaybeUninit;
use limen_treaty::model::{Alignment, ByteLength};
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

fn begin_return(source: &Allocator, material: Retired) -> ReturnId {
    let mut receiver = None;
    source.begin_return(material, &mut receiver).ok().unwrap();
    receiver.unwrap()
}

fn initial_capacity(source: &Allocator) -> u64 {
    let root_pages = CONFIG.layout().unwrap().size().div_ceil(PAGE as usize) as u64;
    assert_eq!(source.root(), pages(4, 4 + root_pages));
    (13 - root_pages) * PAGE
}

#[test]
fn only_the_original_source_can_resolve_region_and_allowance_slots() {
    use crate::metal::sync::TurnWorkspace;

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

    let budget = source
        .budget(Fast)
        .map(|(total, unassigned)| (total.get(), unassigned.get()))
        .unwrap();
    let other_budget = other
        .budget(Fast)
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
        other.lookup(request, Some(region)),
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
        source
            .budget(Fast)
            .map(|(total, unassigned)| (total.get(), unassigned.get())),
        Ok(budget)
    );
    assert_eq!(
        other
            .budget(Fast)
            .map(|(total, unassigned)| (total.get(), unassigned.get())),
        Ok(other_budget)
    );

    allowance.shut(Some(region), Fast).unwrap();
    let (allowance, failure) = allowance.reclaim(Fast).err().unwrap();
    assert_eq!(failure, Error::Balance);
    let receipt = begin_return(source, unsafe { original.retire() });
    let mut cursor = 0;
    assert_eq!(other.pending(region, &mut cursor, 1), Err(Error::Stale));
    assert_eq!(cursor, 0);
    assert_eq!(other.finish_return(receipt, Fast), Err(Error::Stale));
    assert_eq!(
        source.pending(region, &mut cursor, 1),
        Ok(Pending::Found(receipt))
    );
    source.finish_return(receipt, Fast).unwrap();
    allowance.reclaim(Fast).ok().unwrap();
    source.acknowledge(receipt).unwrap();
    assert_eq!(
        other
            .budget(Fast)
            .map(|(total, unassigned)| (total.get(), unassigned.get())),
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

fn metadata_candidate(source: &'static Allocator) -> crate::metal::allocator::Candidate {
    let request = Request::new(
        ByteLength::new(PAGE),
        Alignment::new(PAGE).unwrap(),
        pages(3, 4),
        Some(ATTRIBUTES),
        Metadata,
    )
    .unwrap();
    let Search::Candidate(candidate) = source.lookup(request, None).unwrap().step().unwrap() else {
        panic!("expected the fixture's free metadata page");
    };
    candidate
}

#[test]
fn initial_pend_limits_and_revalidates_the_original_spender() {
    use crate::metal::{
        allocator::{AllocationConstruction, RamPurpose},
        sync::TurnWorkspace,
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
            let mut lookup = source.lookup(request, None).unwrap();
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
    let retired = unsafe { material.unwrap().retire() };
    unsafe { retired.begin_return(&mut receipt) }.ok().unwrap();
    let receipt = receipt.unwrap();
    assert_eq!(receipt.finish(Fast).unwrap().allocation, issued_id);
    assert_eq!(receipt.finish(Fast).unwrap().allocation, issued_id);
    receipt.acknowledge().unwrap();
    assert_eq!(receipt.finish(Fast), Err(Error::Stale));
    assert_eq!(allowance.balance(), Ok(before));
}

#[test]
fn reserve_and_revocation_order_the_original_holding() {
    use crate::metal::allocator::AllocationConstruction;
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
    use crate::metal::allocator::AllocationConstruction;
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
            let receipt = begin_return(source, unsafe { material.unwrap().retire() });
            assert_eq!(source.finish_return(receipt, Slow).unwrap().allocation, id);
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
        source
            .budget(Fast)
            .map(|(total, unassigned)| (total.get(), unassigned.get())),
        Ok((capacity, capacity - before.occupied().get()))
    );
}

#[test]
fn issue_and_cancellation_have_one_winner() {
    use crate::metal::allocator::AllocationConstruction;
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
            let receipt = begin_return(source, unsafe { material.unwrap().retire() });
            assert_eq!(source.finish_return(receipt, Slow).unwrap().allocation, id);
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
        source
            .budget(Slow)
            .map(|(total, unassigned)| (total.get(), unassigned.get())),
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

    let receipt = begin_return(source, unsafe { merged.retire() });
    let mut cursor = 0;
    loop {
        match source.pending(region, &mut cursor, 1).unwrap() {
            Pending::Found(id) => {
                assert_eq!(id, receipt);
                break;
            }
            Pending::More => {}
            Pending::End => panic!("lost return"),
        }
    }
    allowance.shut(Some(credit), Fast).unwrap();
    let done = source.finish_return(receipt, Slow).unwrap();
    assert_eq!(done.physical, pages(0, 3));
    assert_eq!(
        source
            .budget(Fast)
            .map(|(total, unassigned)| (total.get(), unassigned.get())),
        Ok((capacity, capacity))
    );
    allowance.reclaim(Slow).ok().unwrap();
    assert_eq!(source.finish_return(receipt, Fast), Ok(done));
    source.acknowledge(receipt).unwrap();
    assert_eq!(source.finish_return(receipt, Fast), Err(Error::Stale));
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
    let receipt = begin_return(source, unsafe { returned.retire() });
    let result = source.finish_return(receipt, Fast).unwrap();
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
    assert_eq!(source.finish_return(receipt, Fast), Ok(result));

    source.acknowledge(receipt).unwrap();
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
        let receipt = begin_return(source, unsafe { material.retire() });
        source.finish_return(receipt, Fast).unwrap();
        source.acknowledge(receipt).unwrap();
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
    let mut lookup = source.lookup(request, None).unwrap();
    loop {
        match lookup.step().unwrap() {
            Search::Pending => {}
            Search::NoRange => break,
            Search::Candidate(_) => panic!("occupied or too short"),
        }
    }

    let mut lookup = source.lookup(request, Some(left)).unwrap();
    // Observe the negative left Region, then make a qualifying range free.
    assert!(matches!(lookup.step(), Ok(Search::Pending)));
    let material = initial.take().unwrap();
    let id = begin_return(source, unsafe { material.retire() });
    source.finish_return(id, Slow).unwrap();
    loop {
        match lookup.step() {
            Ok(Search::Pending) => {}
            Err(Error::Changed) => break,
            _ => panic!("returned a stale negative"),
        }
    }
    let mut positive = source.lookup(request, None).unwrap();
    let Search::Candidate(candidate) = positive.step().unwrap() else {
        panic!("the stable freed range was not found");
    };
    assert!(candidate.physical().end() <= pages(0, 4).end());
    assert_eq!(candidate.region(), left);

    let later = pages(16, 20);
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
        )
        .unwrap();
    let description = RegionDescription {
        physical: later,
        origin: ATTRIBUTES,
    };
    let budget_before_grant = source
        .budget(Fast)
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
        source
            .budget(Fast)
            .map(|(total, unassigned)| (total.get(), unassigned.get())),
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
        source
            .budget(Fast)
            .map(|(total, unassigned)| (total.get(), unassigned.get())),
        Ok((capacity, capacity - 3 * PAGE))
    );
    assert_eq!(allowance.balance().unwrap().occupied().get(), 0);
}

#[test]
fn reservation_and_cancellation_use_the_complete_plan_under_contention() {
    use crate::metal::{allocator::AllocationConstruction, sync::TurnWorkspace};
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
    let mut lookup = source.lookup(request, None).unwrap();
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
        source
            .budget(Fast)
            .map(|(total, unassigned)| (total.get(), unassigned.get())),
        Ok((capacity, capacity - 3 * PAGE))
    );
}

#[test]
fn initial_allowance_can_shut_and_reclaim_without_the_boot_result() {
    use crate::metal::{allocator::AllocationConstruction, sync::TurnWorkspace};
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
        source
            .budget(Fast)
            .map(|(total, unassigned)| (total.get(), unassigned.get())),
        Ok((capacity, capacity - 6 * PAGE))
    );

    allowance.shut(None, Slow).unwrap();
    let material = initial.take().unwrap();
    let receipt = begin_return(source, unsafe { material.retire() });
    source.finish_return(receipt, Fast).unwrap();
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
        source.finish_return(receipt, Fast).unwrap().physical,
        pages(0, 3)
    );
}

#[test]
fn lost_return_response_replays_after_active_slot_reuse_and_history_saturation() {
    use crate::metal::allocator::AllocationConstruction;

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
    let mut receipts: [Option<ReturnId>; CONFIG.returns] = [None; CONFIG.returns];
    // The caller loses the response. Recovery discovers the Pending record
    // independently and does not read the original owner's receipt array.
    let _lost = source.begin_return(unsafe { original.retire() }, &mut receipts[0]);
    let Pending::Found(pending) = source.pending(region, &mut 0, CONFIG.returns).unwrap() else {
        panic!("lost response made the return undiscoverable");
    };
    let original_return = source.finish_return(pending, Fast).unwrap();
    assert_eq!(original_return.allocation, original_id);
    let owner_receipt = receipts[0].unwrap();
    assert_eq!(owner_receipt, pending);
    let request = Request::new(
        ByteLength::new(PAGE),
        Alignment::new(PAGE).unwrap(),
        pages(0, 1),
        None,
        Metadata,
    )
    .unwrap();

    for receipt in &mut receipts[1..] {
        let Search::Candidate(candidate) = source.lookup(request, None).unwrap().step().unwrap()
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
                .finish_return(owner_receipt, Fast)
                .unwrap()
                .allocation,
            original_id
        );
        assert_eq!(allowance.balance().unwrap().occupied().get(), PAGE);
        source
            .begin_return(unsafe { issued.retire() }, receipt)
            .ok()
            .unwrap();
        source.finish_return(receipt.unwrap(), Slow).unwrap();
    }

    let Search::Candidate(candidate) = source.lookup(request, None).unwrap().step().unwrap() else {
        panic!("metadata saturation cannot rule out a physical range");
    };
    let construction = &*Box::leak(Box::new(AllocationConstruction::new()));
    assert_eq!(
        spending.reserve(candidate, construction, Fast),
        Err(PendError::Source(Error::ReturnCapacity))
    );
    assert_eq!(allowance.balance().unwrap().available().get(), 8 * PAGE);

    source.acknowledge(owner_receipt).unwrap();
    let reserved = spending.reserve(candidate, construction, Slow).unwrap();
    assert_eq!(spending.issue(construction, Slow), Ok(reserved));
    assert_eq!(source.finish_return(owner_receipt, Fast), Err(Error::Stale));
    let mut issued = None;
    unsafe { construction.take(&mut issued) }.unwrap();
    let issued = issued.unwrap();
    let mut occupied_receiver = Some(owner_receipt);
    let (retired, error) = source
        .begin_return(unsafe { issued.retire() }, &mut occupied_receiver)
        .err()
        .unwrap();
    assert_eq!(error, Error::Receiver);
    assert_eq!(occupied_receiver, Some(owner_receipt));
    assert_eq!(
        source.pending(region, &mut 0, CONFIG.returns),
        Ok(Pending::End)
    );
    let new_receipt = begin_return(source, retired);
    source.finish_return(new_receipt, Fast).unwrap();
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
    let mut first = source.lookup(request, None).unwrap();
    let mut second = source.lookup(request, None).unwrap();
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
