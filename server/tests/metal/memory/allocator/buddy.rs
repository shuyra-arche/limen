use core::{mem::MaybeUninit, ops::Range};
use limen_treaty::model::{Alignment, ByteLength, PhysicalAddress, PhysicalRange};

use crate::metal::memory::allocator::region::{
    Revision,
    buddy::{Buddy, Error, Geometry, Request, Root, Scan, Search},
};

fn physical(range: Range<u64>) -> PhysicalRange {
    PhysicalRange::new(
        PhysicalAddress::new(range.start),
        PhysicalAddress::new(range.end),
    )
    .unwrap()
}

fn request(length: u64, alignment: u64) -> Request {
    Request::new(ByteLength::new(length), Alignment::new(alignment).unwrap()).unwrap()
}

fn with_forest<R>(
    range: Range<u64>,
    granule: u64,
    run: impl FnOnce(&mut Buddy<'_>, &Geometry) -> R,
) -> R {
    let geometry = Geometry::new(physical(range), ByteLength::new(granule)).unwrap();
    let mut roots = [const { MaybeUninit::<Root>::uninit() }; 16];
    let mut nodes = [MaybeUninit::uninit(); 128];
    let mut buddy = Buddy::new(
        geometry.clone(),
        &mut roots,
        &mut nodes,
        geometry.edit_bound(),
    )
    .unwrap();
    run(&mut buddy, &geometry)
}

fn seed(buddy: &mut Buddy<'_>, base: u64, count: u64, granule: u64, occupied: u64) {
    for grain in 0..count {
        if occupied & (1 << grain) != 0 {
            let address = (base + grain) * granule;
            buddy.occupy(physical(address..address + granule)).unwrap();
        }
    }
}

fn free(occupied: u64, first: u64, count: u64) -> bool {
    let mask = ((1 << count) - 1) << first;
    occupied & mask == 0
}

fn flat_candidate(
    base: u64,
    count: u64,
    occupied: u64,
    length: u64,
    alignment: u64,
    granule: u64,
) -> Option<Range<u64>> {
    if length > count {
        return None;
    }
    for offset in 0..=count - length {
        let start = (base + offset) * granule;
        if start.is_multiple_of(alignment) && free(occupied, offset, length) {
            return Some(start..start + length * granule);
        }
    }
    None
}

fn complete_search(
    buddy: &Buddy<'_>,
    geometry: &Geometry,
    request: Request,
    batch: usize,
) -> Option<PhysicalRange> {
    let mut observation = buddy.scan(request, 0).unwrap();
    let mut remaining = geometry.scan_bound();
    loop {
        match observation {
            Search::Found(physical) => return Some(physical),
            Search::Missing(revision) => {
                assert_eq!(revision, buddy.revision());
                return None;
            }
            Search::Pending(scan) => {
                assert!(remaining != 0, "scan exceeded the node/advance bound");
                let step = batch.min(remaining);
                observation = buddy.resume(scan, step).unwrap();
                remaining -= step;
            }
        }
    }
}

#[test]
fn geometries_and_fragmented_search_match_the_flat_model() {
    const GRANULE: u64 = 16;
    for base in 0..8 {
        for count in 1..=7 {
            for occupied in 0..1 << count {
                with_forest(
                    base * GRANULE..(base + count) * GRANULE,
                    GRANULE,
                    |buddy, geometry| {
                        seed(buddy, base, count, GRANULE, occupied);
                        let largest = (0..=3).rev().find(|order| {
                            let size = 1u64 << order;
                            flat_candidate(base, count, occupied, size, size * GRANULE, GRANULE)
                                .is_some()
                        });
                        assert_eq!(buddy.largest_free_order(), largest);
                        for length in 1..=count + 1 {
                            for alignment in [1, 8, 16, 32, 64, 128] {
                                let request = request(length * GRANULE, alignment);
                                let expected = flat_candidate(
                                    base, count, occupied, length, alignment, GRANULE,
                                );
                                let batch = 1 + (occupied as usize % 5);
                                assert_eq!(
                                    complete_search(buddy, geometry, request, batch),
                                    expected.map(physical)
                                );

                                let block = length.max(alignment / GRANULE).next_power_of_two();
                                let summary_hit = flat_candidate(
                                    base,
                                    count,
                                    occupied,
                                    block,
                                    block * GRANULE,
                                    GRANULE,
                                );
                                let fast = buddy.fast(request).unwrap();
                                assert_eq!(fast.is_some(), summary_hit.is_some());
                                if let Some(found) = fast {
                                    let start = found.start().get();
                                    let end = found.end().get();
                                    assert_eq!(found.length(), ByteLength::new(length * GRANULE));
                                    assert_eq!(start % alignment, 0);
                                    assert!(start / GRANULE >= base);
                                    assert!(end / GRANULE <= base + count);
                                    assert!(free(occupied, start / GRANULE - base, length));
                                }
                            }
                        }
                    },
                );
            }
        }
    }
}

fn check_cells(buddy: &mut Buddy<'_>, base: u64, count: u64, occupied: u64) {
    for offset in 0..count {
        let address = base + offset;
        let result = buddy.occupy(physical(address..address + 1));
        if free(occupied, offset, 1) {
            result.unwrap();
            buddy.release(physical(address..address + 1)).unwrap();
        } else {
            assert_eq!(result, Err(Error::State));
        }
    }
}

#[test]
fn exact_edits_are_all_or_nothing_and_fit_the_visit_bound() {
    for base in 0..5 {
        for count in 1..=6 {
            for occupied in 0..1 << count {
                with_forest(base..base + count, 1, |buddy, geometry| {
                    seed(buddy, base, count, 1, occupied);
                    for first in 0..count {
                        for end in first + 1..=count {
                            let physical = physical(base + first..base + end);
                            let mask = ((1 << (end - first)) - 1) << first;
                            for releasing in [false, true] {
                                let before = buddy.revision();
                                let expected = if releasing {
                                    occupied & mask == mask
                                } else {
                                    occupied & mask == 0
                                };
                                let result = if releasing {
                                    buddy.release(physical)
                                } else {
                                    buddy.occupy(physical)
                                };
                                if expected {
                                    assert!(result.unwrap() <= geometry.edit_bound());
                                    assert_eq!(buddy.revision(), before.next());
                                    check_cells(buddy, base, count, occupied ^ mask);
                                    if releasing {
                                        buddy.occupy(physical).unwrap();
                                    } else {
                                        buddy.release(physical).unwrap();
                                    }
                                } else {
                                    assert_eq!(result, Err(Error::State));
                                    assert_eq!(buddy.revision(), before);
                                    check_cells(buddy, base, count, occupied);
                                }
                            }
                        }
                    }
                });
            }
        }
    }
}

#[test]
fn an_exact_three_granule_run_can_cross_roots() {
    with_forest(3..8, 1, |buddy, geometry| {
        // Absolute roots are [3, 4) and [4, 8).
        assert_eq!(geometry.root_count(), 2);
        assert_eq!(geometry.node_count(), 8);
        buddy.occupy(physical(6..8)).unwrap();
        let request = request(3, 1);
        assert_eq!(buddy.largest_free_order(), Some(1));
        assert_eq!(buddy.fast(request).unwrap(), None);
        assert_eq!(
            complete_search(buddy, geometry, request, 1),
            Some(physical(3..6))
        );
        assert_eq!(buddy.occupy(physical(2..4)), Err(Error::Range));
        assert_eq!(buddy.occupy(physical(7..9)), Err(Error::Range));
        buddy.occupy(physical(3..6)).unwrap();
        assert_eq!(buddy.largest_free_order(), None);
        buddy.release(physical(3..6)).unwrap();
        assert_eq!(
            complete_search(buddy, geometry, request, 2),
            Some(physical(3..6))
        );
    });
}

fn pending<'storage>(search: Search<'storage>) -> Scan<'storage> {
    let Search::Pending(scan) = search else {
        panic!("expected saved progress");
    };
    scan
}

#[test]
fn progress_belongs_to_one_current_region_and_a_failed_edit_keeps_it_valid() {
    let request = request(3, 1);
    with_forest(3..8, 1, |buddy, _| {
        let scan = pending(buddy.scan(request, 1).unwrap());
        with_forest(3..8, 1, |other, _| {
            assert!(matches!(other.resume(scan, 20), Err(Error::Foreign)));
        });
        let scan = pending(buddy.scan(request, 1).unwrap());
        assert_eq!(buddy.release(physical(4..5)), Err(Error::State));
        assert!(
            matches!(buddy.resume(scan, 20), Ok(Search::Found(found)) if found == physical(3..6))
        );

        let scan = pending(buddy.scan(request, 1).unwrap());
        buddy.occupy(physical(3..4)).unwrap();
        buddy.release(physical(3..4)).unwrap();
        assert!(matches!(buddy.resume(scan, 20), Err(Error::Changed)));
    });
}

#[test]
fn exhausted_revision_cannot_certify_resumed_progress() {
    let final_revision = Revision::Current(u64::MAX);
    assert!(final_revision.preserves(final_revision));
    let exhausted = final_revision.next();
    assert_eq!(exhausted, Revision::Exhausted);
    assert_eq!(exhausted.next(), exhausted);
    assert!(!exhausted.preserves(final_revision));
    assert!(!exhausted.preserves(exhausted));
}

#[test]
fn construction_rejects_bad_endpoints_overflow_and_short_prepaid_storage() {
    assert!(PhysicalRange::new(PhysicalAddress::new(0), PhysicalAddress::new(0)).is_none());
    assert_eq!(
        Geometry::new(physical(0..10), ByteLength::new(4)),
        Err(Error::Range)
    );
    assert_eq!(
        Geometry::new(physical(0..8), ByteLength::new(3)),
        Err(Error::Granule)
    );
    assert_eq!(
        Geometry::new(physical(0..u64::MAX), ByteLength::new(1)),
        Err(Error::Capacity)
    );

    let geometry = Geometry::new(physical(3..8), ByteLength::new(1)).unwrap();
    let mut roots = [const { MaybeUninit::<Root>::uninit() }; 4];
    let mut nodes = [MaybeUninit::new(0xa5); 9];
    assert!(matches!(
        Buddy::new(
            geometry.clone(),
            &mut roots,
            &mut nodes,
            geometry.edit_bound() - 1
        ),
        Err(Error::EditLimit)
    ));
    assert!(matches!(
        Buddy::new(
            geometry.clone(),
            &mut roots[..1],
            &mut nodes,
            geometry.edit_bound()
        ),
        Err(Error::Capacity)
    ));
    assert!(matches!(
        Buddy::new(
            geometry.clone(),
            &mut roots,
            &mut nodes[..7],
            geometry.edit_bound()
        ),
        Err(Error::Capacity)
    ));
    // Every byte started initialized and all three preflights rejected.
    assert!(
        nodes
            .iter()
            .all(|node| unsafe { node.assume_init() } == 0xa5)
    );
    Buddy::new(
        geometry.clone(),
        &mut roots,
        &mut nodes,
        geometry.edit_bound(),
    )
    .unwrap();
    assert_eq!(unsafe { nodes[8].assume_init() }, 0xa5);
}

#[test]
fn reports_missing_when_rounding_or_candidate_end_exceeds_physical_addresses() {
    let start = (1u64 << 63) + 16;
    with_forest(start..start + 64, 4, |buddy, geometry| {
        assert_eq!(
            Request::new(ByteLength::new(0), Alignment::new(1).unwrap()),
            Err(Error::Layout)
        );
        assert!(Alignment::new(3).is_none());
        assert_eq!(buddy.fast(request(3, 1)), Err(Error::Layout));
        let high_alignment = request(4, 1u64 << 63);
        assert_eq!(buddy.fast(high_alignment).unwrap(), None);
        assert_eq!(complete_search(buddy, geometry, high_alignment, 1), None);
        let high_length = request(u64::MAX - 3, 1);
        assert_eq!(buddy.fast(high_length).unwrap(), None);
        assert_eq!(complete_search(buddy, geometry, high_length, 1), None);
    });
}

#[test]
fn lookup_clips_an_aligned_candidate_to_its_access_window() {
    let geometry = Geometry::new(physical(0..64), ByteLength::new(1)).unwrap();
    let mut roots = [const { MaybeUninit::uninit() }; 1];
    let mut nodes = [const { MaybeUninit::uninit() }; 127];
    let buddy = Buddy::new(geometry, &mut roots, &mut nodes, 64).unwrap();
    let request = request(5, 4).restrict(physical(7..14));
    assert_eq!(request.length(), ByteLength::new(5));
    assert_eq!(request.alignment(), Alignment::new(4).unwrap());
    assert_eq!(request.window(), Some(physical(7..14)));
    assert_eq!(buddy.fast(request).unwrap(), Some(physical(8..13)));
    let request = request.restrict(physical(0..12));
    assert!(buddy.fast(request).unwrap().is_none());
    let disjoint = request.restrict(physical(12..32));
    assert_eq!(disjoint.window(), None);
    assert!(buddy.fast(disjoint).unwrap().is_none());
    assert!(matches!(
        buddy.scan(request, 16).unwrap(),
        Search::Missing(_)
    ));
}

#[test]
fn both_search_paths_return_an_occupiable_grain_inside_a_clipped_window() {
    with_forest(0..16_384, 4096, |buddy, geometry| {
        let request = request(4096, 1).restrict(physical(1..12_288));
        let fast = buddy.fast(request).unwrap().unwrap();
        assert_eq!(fast, physical(4096..8192));
        buddy.occupy(fast).unwrap();
        buddy.release(fast).unwrap();

        let scanned = complete_search(buddy, geometry, request, 1).unwrap();
        assert_eq!(scanned, physical(4096..8192));
        buddy.occupy(scanned).unwrap();
    });
}

#[test]
fn delayed_initialization_touches_only_actual_roots() {
    const PAGE: u64 = 4096;
    const INACTIVE: u8 = 0xa5;
    let geometry = Geometry::new(physical(3 * PAGE..515 * PAGE), ByteLength::new(PAGE)).unwrap();
    let count = geometry.root_count();
    assert!(geometry.node_count() > 32 * count);
    let mut roots = [const { MaybeUninit::<Root>::uninit() }; 128];
    let mut nodes = [INACTIVE; 1024];

    {
        // The real Allocator path supplies initialized storage. Inactive
        // descendants have no semantic state until a parent is split.
        let buddy = Buddy::initialize(
            geometry.clone(),
            &mut roots,
            &mut nodes,
            geometry.edit_bound(),
        )
        .unwrap();
        assert!(buddy.fast(request(PAGE, PAGE)).unwrap().is_some());
    }
    assert_eq!(
        nodes.iter().filter(|&&byte| byte != INACTIVE).count(),
        count
    );
    assert_eq!(nodes.iter().filter(|&&byte| byte == 1).count(), count);

    // A near-full address-space geometry does not need a backing forest
    // for this pure bound check.
    let wide = Geometry::new(
        physical(PAGE..(u64::MAX & !(PAGE - 1))),
        ByteLength::new(PAGE),
    )
    .unwrap();
    assert!(wide.root_count() <= 2 * u64::BITS as usize);
}
