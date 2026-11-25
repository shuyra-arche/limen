use crate::{
    Boot, FirmwareMap, IMAGE, Physical, Retirement,
    allocator::{
        Allocation, Config, Error, Founded, Grant, Initial, Pending, RamPurpose::Metadata, Request,
        Search,
    },
    describe,
    organization::retired,
    regions::{MappedRange, Region, RegionSource, Regions},
    sync::Acquisition::{Fast, Slow},
};
use core::mem::MaybeUninit;

const PAGE: u64 = 4096;
const BASE: u64 = 0x100_000;
const SCOPE: Physical = pages(0, 13);
const LATER: Physical = pages(16, 20);
const ATTRIBUTES: RegionSource = RegionSource::Uefi { attributes: 8 };
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

const fn pages(first: u64, last: u64) -> Physical {
    Physical {
        start: BASE + first * PAGE,
        end: BASE + last * PAGE,
    }
}

#[repr(align(4096))]
struct Mapped([MaybeUninit<u8>; 2 * PAGE as usize]);

#[repr(align(8))]
struct Descriptors([u8; 40]);

fn bootstrap(operation: impl FnOnce(&mut Regions<'_>, &mut Option<MappedRange<'static>>)) {
    let mut descriptors = Descriptors([0; 40]);
    describe(&mut descriptors.0, 7, SCOPE.start, 13, 8);
    let map = FirmwareMap::new(&descriptors.0, 40, 1, 1).unwrap();
    let boot = Boot::from_uefi(0, IMAGE, None, map, None);
    let exclusions = boot.exclusions_for(IMAGE, IMAGE);
    let evidence = retired(SCOPE);
    let mut scratch = [None; 3];
    let mut regions =
        Regions::organize(&boot, &exclusions, PAGE, Some(&evidence), &mut scratch).unwrap();
    let backing = Box::leak(Box::new(Mapped([MaybeUninit::uninit(); 2 * PAGE as usize])));
    let mut mapping = Some(unsafe { MappedRange::new(pages(4, 6), &mut backing.0) }.unwrap());
    operation(&mut regions, &mut mapping);
}

fn source(initial: &mut Option<Allocation>, capacity: u64) -> Founded {
    let mut founded = None;
    bootstrap(|regions, mapping| {
        founded = unsafe {
            regions.allocator(
                mapping,
                CONFIG,
                capacity,
                &mut [Initial::new(pages(0, 3), initial)],
            )
        }
        .unwrap();
        assert!(mapping.is_none());
        assert_eq!(regions.entries().count(), 0);
    });
    founded.unwrap()
}

#[test]
fn rejects_invalid_geometry_and_receivers_before_consuming_the_root() {
    bootstrap(|regions, mapping| {
        let mut initial = None;
        let input = &mut [Initial::new(pages(3, 5), &mut initial)];
        assert_eq!(
            unsafe { regions.allocator(mapping, CONFIG, 3 * PAGE, input) }.err(),
            Some(Error::Range)
        );
        assert!(mapping.is_some() && initial.is_none());
        assert_eq!(regions.entries().next().unwrap().physical, SCOPE);
        let config = Config {
            regions: 1,
            ..CONFIG
        };
        assert_eq!(
            unsafe { regions.allocator(mapping, config, 0, &mut []) }.err(),
            Some(Error::Capacity)
        );
        assert!(mapping.is_some());
        assert!(
            unsafe { regions.allocator(&mut None, CONFIG, 0, &mut []) }
                .unwrap()
                .is_none()
        );
    });
}

#[test]
fn independent_history_survives_new_generations_and_refunds_after_shut() {
    let mut initial = None;
    let Founded { source, allowance } = source(&mut initial, 10 * PAGE);
    assert_eq!(source.root(), pages(4, 6));
    assert_eq!(source.budget(Slow), Ok((11 * PAGE, PAGE)));
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
    let unused = original.physical().end;
    unsafe { source.split(original.retire(), unused - PAGE, &mut halves, Fast) }
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
    assert_eq!(allowance.balance().unwrap().occupied, 3 * PAGE);

    let receipt = source
        .begin_return(unsafe { merged.retire() })
        .ok()
        .unwrap();
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
    assert_eq!(source.budget(Fast), Ok((11 * PAGE, 11 * PAGE)));
    allowance.reclaim(Slow).ok().unwrap();
    assert_eq!(source.finish_return(receipt, Fast), Ok(done));
    source.acknowledge(receipt).unwrap();
    assert_eq!(source.finish_return(receipt, Fast), Err(Error::Stale));
}

#[test]
fn negative_search_checks_both_passes_and_rejects_new_geometry() {
    let mut initial = None;
    let Founded { source, allowance } = source(&mut initial, 3 * PAGE);
    let left = initial.as_ref().unwrap().region();
    let request = Request::new(2 * PAGE, PAGE, pages(0, 4), Some(ATTRIBUTES), Metadata).unwrap();
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
    let id = source
        .begin_return(unsafe { material.retire() })
        .ok()
        .unwrap();
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
    assert!(candidate.physical().end <= pages(0, 4).end);
    assert_eq!(candidate.region(), left);

    let mut adding = source
        .lookup(
            Request::new(PAGE, PAGE, LATER, None, Metadata).unwrap(),
            None,
        )
        .unwrap();
    let description = Region {
        physical: LATER,
        source: ATTRIBUTES,
    };
    assert_eq!(
        unsafe { Grant::new(source, description, &Retirement::new(LATER).unwrap()) }.err(),
        Some(Error::Range)
    );
    let mut grant = unsafe { Grant::new(source, description, &retired(LATER)) }.unwrap();
    let region = grant.admit(Fast).unwrap();
    assert_eq!(grant.admit(Slow), Ok(region));
    loop {
        match adding.step() {
            Ok(Search::Pending) => {}
            Err(Error::Changed) => break,
            _ => panic!("missed a new published Region"),
        }
    }
    assert_eq!(source.budget(Fast), Ok((15 * PAGE, 12 * PAGE)));
    assert_eq!(allowance.balance().unwrap().occupied, 0);
}

#[test]
fn reservation_and_cancellation_use_the_complete_plan_under_contention() {
    use crate::{
        allocator::Construction,
        sync::{AcquisitionError, TurnLock, Work},
    };
    use std::{sync::mpsc::sync_channel, thread};

    // The Host owner explicitly funds its single actual construction, with a
    // revocable grant limited to its exact allowance, layout and purpose.
    let mut initial = None;
    let Founded { source, allowance } = source(&mut initial, 8 * PAGE);
    let id = allowance.id();
    let spending = TurnLock::new(Some(id));
    let construction = Box::leak(Box::new(TurnLock::new(Construction::new())));
    let construction = &*construction;
    let request = Request::new(PAGE, PAGE, pages(3, 4), Some(ATTRIBUTES), Metadata).unwrap();
    let mut lookup = source.lookup(request, None).unwrap();
    let Search::Candidate(candidate) = lookup.step().unwrap() else {
        panic!("free granule")
    };

    let (hold, held) = sync_channel(0);
    let (release, released) = sync_channel(0);
    thread::scope(|scope| {
        scope.spawn(move || {
            let mut work = Work::<1>::new();
            let mut plan = work.plan();
            plan.insert(construction).unwrap();
            plan.execute(Fast, |_| {
                hold.send(()).unwrap();
                released.recv().unwrap();
            })
            .unwrap();
        });
        held.recv().unwrap();
        let mut work = Work::<4>::new();
        let mut plan = work.plan();
        source
            .plan_construction(&mut plan, candidate.region(), id, construction)
            .unwrap();
        plan.insert(&spending).unwrap();
        let result = plan.execute(Fast, |_| panic!("partially held commit"));
        release.send(()).unwrap();
        assert_eq!(result, Err::<(), _>(AcquisitionError::Busy));
        assert_eq!(allowance.balance().unwrap().reserved, 0);
    });

    let mut work = Work::<4>::new();
    let mut plan = work.plan();
    source
        .plan_construction(&mut plan, candidate.region(), id, construction)
        .unwrap();
    plan.insert(&spending).unwrap();
    plan.execute(Slow, |locks| {
        assert_eq!(*locks.access(&spending).unwrap(), Some(id));
        assert_eq!(candidate.request().length(), PAGE);
        assert_eq!(candidate.request().alignment(), PAGE);
        assert_eq!(candidate.request().purpose(), Metadata);
        unsafe { source.reserve(locks, candidate, id, construction) }.unwrap()
    })
    .unwrap();
    assert_eq!(allowance.balance().unwrap().reserved, PAGE);

    let mut plan = work.plan();
    source
        .plan_construction(&mut plan, candidate.region(), id, construction)
        .unwrap();
    plan.insert(&spending).unwrap();
    plan.execute(Slow, |locks| {
        assert_eq!(
            source
                .reserved_candidate(locks, construction)
                .unwrap()
                .physical(),
            pages(3, 4)
        );
        *locks.access(&spending).unwrap() = None;
        locks.access(construction).unwrap().close();
    })
    .unwrap();
    allowance.shut(Some(candidate.region()), Fast).unwrap();
    let mut plan = work.plan();
    source
        .plan_construction(&mut plan, candidate.region(), id, construction)
        .unwrap();
    plan.execute(Slow, |locks| source.cancel(locks, construction))
        .unwrap()
        .unwrap();
    assert_eq!(allowance.balance().unwrap().reserved, 0);
    assert_eq!(source.budget(Fast), Ok((11 * PAGE, 8 * PAGE)));
}

#[test]
fn a_retired_allowance_reuses_its_slot_without_reusing_its_identity() {
    use crate::sync::Work;
    let mut initial = None;
    let Founded { source, allowance } = source(&mut initial, 3 * PAGE);
    let original = allowance.id();
    let region = initial.as_ref().unwrap().region();
    let (_, credit) = source.regions().nth(1).unwrap();
    assert_eq!(credit.physical, pages(6, 13));
    let other = source.regions().nth(1).unwrap().0;
    let debits = [(region, PAGE), (other, 2 * PAGE)];

    let mut receiver = None;
    let index = source.allowance_candidate().unwrap();
    let mut work = Work::<5>::new();
    let mut plan = work.plan();
    source.plan_allowance(&mut plan, index, &debits).unwrap();
    plan.execute(Slow, |held| {
        // This fixture is the exclusive source assignment owner and keeps
        // each close responsibility in its exact receiving position.
        unsafe { source.open_allowance(held, index, &debits, &mut receiver) }
    })
    .unwrap()
    .unwrap();
    let second = receiver.take().unwrap();
    assert_eq!(second.balance().unwrap().capacity, 3 * PAGE);
    assert_eq!(source.allowance_candidate(), Err(Error::AllowanceCapacity));
    assert_eq!(source.budget(Fast), Ok((11 * PAGE, 5 * PAGE)));

    allowance.shut(None, Slow).unwrap();
    let material = initial.take().unwrap();
    let receipt = source
        .begin_return(unsafe { material.retire() })
        .ok()
        .unwrap();
    source.finish_return(receipt, Fast).unwrap();
    allowance.reclaim(Slow).ok().unwrap();
    let index = source.allowance_candidate().unwrap();
    let mut plan = work.plan();
    source.plan_allowance(&mut plan, index, &debits).unwrap();
    plan.execute(Fast, |held| unsafe {
        source.open_allowance(held, index, &debits, &mut receiver)
    })
    .unwrap()
    .unwrap();
    let next = receiver.take().unwrap();
    assert_ne!(next.id(), original);

    let mut plan = work.plan();
    source
        .plan_transfer(&mut plan, original, second.id())
        .unwrap();
    assert_eq!(
        plan.execute(Fast, |held| {
            // A stale role must fail the live Allowance check before either debit.
            unsafe { source.transfer_allowance(held, original, second.id(), PAGE) }
        }),
        Ok(Err(Error::Stale))
    );
    assert_eq!(second.balance().unwrap().capacity, 3 * PAGE);
    assert_eq!(next.balance().unwrap().capacity, 3 * PAGE);
    assert_eq!(
        source.finish_return(receipt, Fast).unwrap().physical,
        pages(0, 3)
    );
}

#[test]
fn issued_material_and_old_return_history_remain_independent_when_slots_fill() {
    use crate::{
        allocator::{Construction, ReturnId},
        sync::{TurnLock, Work},
    };

    let mut initial = None;
    let Founded { source, allowance } = source(&mut initial, 8 * PAGE);
    let id = allowance.id();
    let authority = TurnLock::new(id);
    let original = initial.take().unwrap();
    let original_id = original.id();
    let mut oldest = source
        .begin_return(unsafe { original.retire() })
        .ok()
        .unwrap();
    assert_eq!(
        source.finish_return(oldest, Fast).unwrap().allocation,
        original_id
    );
    let mut receipts: [Option<ReturnId>; CONFIG.returns] = [None; CONFIG.returns];
    receipts[0] = Some(oldest);
    let request = Request::new(PAGE, PAGE, pages(0, 1), None, Metadata).unwrap();

    for receipt in &mut receipts[1..] {
        let Search::Candidate(candidate) = source.lookup(request, None).unwrap().step().unwrap()
        else {
            panic!("exact grain was not refunded");
        };
        let construction = &*Box::leak(Box::new(TurnLock::new(Construction::new())));
        let mut work = Work::<4>::new();
        let mut plan = work.plan();
        source
            .plan_construction(&mut plan, candidate.region(), id, construction)
            .unwrap();
        plan.insert(&authority).unwrap();
        let issued = plan
            .execute(Slow, |held| {
                assert_eq!(*held.access(&authority).unwrap(), id);
                assert_eq!(candidate.request().purpose(), Metadata);
                let reserved =
                    unsafe { source.reserve(held, candidate, id, construction) }.unwrap();
                assert_eq!(unsafe { source.issue(held, construction) }, Ok(reserved));
                unsafe { held.access(construction).unwrap().take() }.unwrap()
            })
            .unwrap();
        assert_ne!(issued.id(), original_id);
        assert_eq!(issued.physical(), pages(0, 1));
        assert_eq!(
            source.finish_return(oldest, Fast).unwrap().allocation,
            original_id
        );
        assert_eq!(allowance.balance().unwrap().occupied, PAGE);
        let done = source
            .begin_return(unsafe { issued.retire() })
            .ok()
            .unwrap();
        source.finish_return(done, Slow).unwrap();
        *receipt = Some(done);
    }

    let Search::Candidate(candidate) = source.lookup(request, None).unwrap().step().unwrap() else {
        panic!("metadata saturation cannot rule out a physical range");
    };
    let construction = &*Box::leak(Box::new(TurnLock::new(Construction::new())));
    let mut work = Work::<4>::new();
    let mut plan = work.plan();
    source
        .plan_construction(&mut plan, candidate.region(), id, construction)
        .unwrap();
    plan.insert(&authority).unwrap();
    assert_eq!(
        plan.execute(Fast, |held| {
            assert_eq!(*held.access(&authority).unwrap(), id);
            assert_eq!(candidate.request().purpose(), Metadata);
            unsafe { source.reserve(held, candidate, id, construction) }
        }),
        Ok(Err(Error::ReturnCapacity))
    );
    assert_eq!(allowance.balance().unwrap().available(), 8 * PAGE);

    source.acknowledge(oldest).unwrap();
    let mut plan = work.plan();
    source
        .plan_construction(&mut plan, candidate.region(), id, construction)
        .unwrap();
    plan.insert(&authority).unwrap();
    let issued = plan
        .execute(Slow, |held| {
            assert_eq!(*held.access(&authority).unwrap(), id);
            unsafe {
                source.reserve(held, candidate, id, construction).unwrap();
                source.issue(held, construction).unwrap();
                held.access(construction).unwrap().take().unwrap()
            }
        })
        .unwrap();
    assert_eq!(source.finish_return(oldest, Fast), Err(Error::Stale));
    oldest = source
        .begin_return(unsafe { issued.retire() })
        .ok()
        .unwrap();
    source.finish_return(oldest, Fast).unwrap();
    assert_eq!(allowance.balance().unwrap().available(), 8 * PAGE);
}
