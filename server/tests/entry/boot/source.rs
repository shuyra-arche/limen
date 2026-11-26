use crate::{
    Boot, FirmwareMap, IMAGE, Physical, Retirement,
    allocator::{
        Allocation, Allocator, Allowance, Config, Error, Grant, Initial, Pending,
        RamPurpose::Metadata, Request, Retired, ReturnId, Search,
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

pub(super) fn bootstrap(
    operation: impl FnOnce(&mut Regions<'_>, &mut Option<MappedRange<'static>>),
) {
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

#[test]
fn initial_pend_limits_and_revalidates_the_original_spender() {
    use crate::{
        allocator::{Construction, RamPurpose},
        pend::{Error as PendError, Initial as InitialPend, PendSpace},
    };

    let cancelled = Box::leak(Box::new(Construction::new()));
    let issued = Box::leak(Box::new(Construction::new()));
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
                    0,
                    PAGE,
                    InitialPend {
                        spending: &mut spending,
                        closing: &mut closing,
                    },
                    Fast
                ),
                Err(PendError::Limit)
            );
            assert!(spending.is_none() && closing.is_none());
            space
                .publish_initial(
                    &allowance,
                    PAGE,
                    PAGE,
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
                    PAGE,
                    PAGE,
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
                    PAGE,
                    PAGE,
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
            let request = Request::new(length, alignment, pages(6, 13), None, purpose).unwrap();
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
                Err(PendError::Limit)
            );
        }
        assert_eq!(allowance.balance(), Ok(before));

        let valid = candidate(PAGE, PAGE, Metadata);
        handle.reserve(valid, cancelled, Fast).unwrap();
        assert_eq!(cancelled.cancel(Fast), Err(Error::Receiver));
        assert_eq!(
            different.issue(cancelled, Fast),
            Err(PendError::Source(Error::Protection))
        );
        assert_eq!(revoker.revoked(), Ok(false));
        revoker.revoke().unwrap();
        assert_eq!(handle.issue(cancelled, Fast), Err(PendError::Revoked));
        assert_eq!(allowance.balance().unwrap().reserved, PAGE);
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
                    PAGE,
                    PAGE,
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
    cancelled.close().unwrap();
    assert_eq!(cancelled.cancel(Fast), Ok(()));
    assert_eq!(cancelled.cancel(Fast), Ok(()));
    assert_eq!(allowance.balance().unwrap().reserved, 0);
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
        assert_eq!(regions.entries().next().unwrap().physical, SCOPE);
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
    assert_eq!(source.budget(Fast), Ok((11 * PAGE, 11 * PAGE)));
    allowance.reclaim(Slow).ok().unwrap();
    assert_eq!(source.finish_return(receipt, Fast), Ok(done));
    source.acknowledge(receipt).unwrap();
    assert_eq!(source.finish_return(receipt, Fast), Err(Error::Stale));
}

#[test]
fn negative_search_checks_both_passes_and_rejects_new_geometry() {
    let mut initial = None;
    let (source, allowance) = source(&mut initial, 3 * PAGE);
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
    let (source, allowance) = source(&mut initial, 8 * PAGE);
    let id = allowance.id();
    let spending = TurnLock::new(Some(id));
    let construction = Box::leak(Box::new(Construction::new()));
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
            construction.plan(&mut plan).unwrap();
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
        unsafe { source.reserve(locks, candidate, id, construction, None) }.unwrap()
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
        construction.close_held(locks).unwrap();
    })
    .unwrap();
    allowance.shut(Some(candidate.region()), Fast).unwrap();
    construction.cancel(Slow).unwrap();
    assert_eq!(allowance.balance().unwrap().reserved, 0);
    assert_eq!(source.budget(Fast), Ok((11 * PAGE, 8 * PAGE)));
}

#[test]
fn initial_allowance_can_shut_and_reclaim_without_the_boot_result() {
    use crate::sync::Work;
    let mut initial = None;
    let (source, allowance) = source(&mut initial, 3 * PAGE);
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
    let receipt = begin_return(source, unsafe { material.retire() });
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
fn lost_return_response_replays_after_active_slot_reuse_and_history_saturation() {
    use crate::{
        allocator::Construction,
        sync::{TurnLock, Work},
    };

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
    let id = allowance.id();
    let authority = TurnLock::new(id);
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
    let request = Request::new(PAGE, PAGE, pages(0, 1), None, Metadata).unwrap();

    for receipt in &mut receipts[1..] {
        let Search::Candidate(candidate) = source.lookup(request, None).unwrap().step().unwrap()
        else {
            panic!("exact grain was not refunded");
        };
        let construction = &*Box::leak(Box::new(Construction::new()));
        let mut work = Work::<4>::new();
        let mut plan = work.plan();
        source
            .plan_construction(&mut plan, candidate.region(), id, construction)
            .unwrap();
        plan.insert(&authority).unwrap();
        plan.execute(Slow, |held| {
            assert_eq!(*held.access(&authority).unwrap(), id);
            assert_eq!(candidate.request().purpose(), Metadata);
            let reserved =
                unsafe { source.reserve(held, candidate, id, construction, None) }.unwrap();
            assert_eq!(
                unsafe { source.issue(held, construction, None) },
                Ok(reserved)
            );
        })
        .unwrap();
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
        assert_eq!(allowance.balance().unwrap().occupied, PAGE);
        source
            .begin_return(unsafe { issued.retire() }, receipt)
            .ok()
            .unwrap();
        source.finish_return(receipt.unwrap(), Slow).unwrap();
    }

    let Search::Candidate(candidate) = source.lookup(request, None).unwrap().step().unwrap() else {
        panic!("metadata saturation cannot rule out a physical range");
    };
    let construction = &*Box::leak(Box::new(Construction::new()));
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
            unsafe { source.reserve(held, candidate, id, construction, None) }
        }),
        Ok(Err(Error::ReturnCapacity))
    );
    assert_eq!(allowance.balance().unwrap().available(), 8 * PAGE);

    source.acknowledge(owner_receipt).unwrap();
    let mut plan = work.plan();
    source
        .plan_construction(&mut plan, candidate.region(), id, construction)
        .unwrap();
    plan.insert(&authority).unwrap();
    plan.execute(Slow, |held| {
        assert_eq!(*held.access(&authority).unwrap(), id);
        unsafe {
            source
                .reserve(held, candidate, id, construction, None)
                .unwrap();
            source.issue(held, construction, None).unwrap();
        }
    })
    .unwrap();
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
    assert_eq!(allowance.balance().unwrap().available(), 8 * PAGE);
}

#[test]
fn skip_only_advances_the_lookup_with_an_outstanding_candidate() {
    let mut initial = None;
    let (source, _allowance) = source(&mut initial, 3 * PAGE);
    let request = Request::new(PAGE, PAGE, pages(3, 4), None, Metadata).unwrap();
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
