use kernel::{
    entry::boot::{self, Boot},
    metal::{
        memory::allocator::{
            Allocation, Allowance, Config, Error, Initial, LookupObservation, Pending,
            RamPurpose::Metadata, Request, ReturnReceipt, Search,
        },
        sync::TurnMode::{Fast, Slow},
    },
};

use super::{
    foundation::inspect_budget,
    memory::{extent, mapped, retirement},
};

use limen_treaty::model::{Alignment, ByteLength, PhysicalRange};

const PAGE: u64 = 4096;
const BASE: u64 = 0x8400_0000;
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

static mut RECEIVER: Option<Allocation> = None;
static mut ALLOWANCE: Option<Allowance> = None;
static mut RECEIPT: Option<ReturnReceipt> = None;

fn pages(first: u64, last: u64) -> PhysicalRange {
    extent(BASE + first * PAGE, BASE + last * PAGE)
}

pub(super) fn check(boot: &Boot<'_>) {
    let scope = pages(0, 13);
    let backing = pages(4, 6);
    let evidence = retirement(boot, scope);
    let mut scratch = [None; 3];
    let mut regions = boot
        .regions(ByteLength::new(PAGE), Some(&evidence), &mut scratch)
        .unwrap();
    assert!(regions.entries().map(|entry| entry.physical).eq([scope]));
    assert!(
        unsafe { regions.allocator(&mut None, CONFIG, 0, &mut None, &mut []) }
            .unwrap()
            .is_none()
    );

    // Bare RV64 gives this reserved extent a canonical identity mapping.
    let mut mapping = Some(unsafe { mapped(backing) });
    let receiver = unsafe { &mut *(&raw mut RECEIVER) };
    let allowance_receiver = unsafe { &mut *(&raw mut ALLOWANCE) };
    // Simulate loss of the transient boot result. All obligations have
    // already reached the original permanent receiving positions.
    assert!(
        unsafe {
            regions.allocator(
                &mut mapping,
                CONFIG,
                10 * PAGE,
                allowance_receiver,
                &mut [Initial::new(pages(0, 3), receiver)],
            )
        }
        .unwrap()
        .is_some()
    );
    let allowance = allowance_receiver.take().unwrap();
    let source = allowance.source();
    assert!(mapping.is_none() && regions.entries().next().is_none());
    let root_pages = CONFIG.layout().unwrap().size().div_ceil(PAGE as usize) as u64;
    assert_eq!(source.root(), pages(4, 4 + root_pages));
    let capacity = (13 - root_pages) * PAGE;
    assert_eq!(
        inspect_budget(source).map(|(total, unassigned)| (total.get(), unassigned.get())),
        Ok((capacity, capacity - 10 * PAGE))
    );

    let mut lookup_observations = [LookupObservation::EMPTY; 3];
    let mut lookup = source
        .lookup(
            Request::new(
                ByteLength::new(2 * PAGE),
                Alignment::new(PAGE).unwrap(),
                pages(0, 4),
                None,
                Metadata,
            )
            .unwrap(),
            None,
            &mut lookup_observations,
        )
        .unwrap();
    loop {
        match lookup.step().unwrap() {
            Search::Pending => {}
            Search::NoRange => break,
            Search::Candidate(_) => panic!("initial occupied material was exposed as free"),
        }
    }

    let later = pages(16, 20);
    let mut absent_observations = [LookupObservation::EMPTY; 3];
    let mut absent = source
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
            &mut absent_observations,
        )
        .unwrap();
    let evidence = retirement(boot, later);
    let regions = boot
        .regions(ByteLength::new(PAGE), Some(&evidence), &mut scratch)
        .unwrap();
    let description = regions.entries().next().unwrap();
    assert_eq!(description.physical, later);
    let mut grant = unsafe { evidence.grant(source, description) }.unwrap();
    let added = grant.admit(Fast).unwrap();
    assert_eq!(grant.admit(Slow), Ok(added));
    loop {
        match absent.step() {
            Ok(Search::Pending) => {}
            Err(Error::Changed) => break,
            _ => panic!("source negative survived publication"),
        }
    }

    let window = extent(later.start().get() + 1, later.end().get());
    let request = Request::new(
        ByteLength::new(PAGE),
        Alignment::new(1).unwrap(),
        window,
        None,
        Metadata,
    )
    .unwrap();
    let Search::Candidate(aligned) = source
        .lookup(request, Some(added), &mut [LookupObservation::EMPTY; 3])
        .unwrap()
        .step()
        .unwrap()
    else {
        panic!("the new Region has an eligible aligned grain");
    };
    assert_eq!(aligned.physical(), pages(17, 18));

    let allocation = receiver.take().unwrap();
    let region = allocation.region();
    // This owner did not expose the allocation or issue any actual access.
    let receipt = unsafe { &mut *(&raw mut RECEIPT) };
    let _lost = unsafe { allocation.retire().begin_return(receipt) };
    let Pending::Found(pending) = source.pending(region, &mut 0, 64).unwrap() else {
        panic!("lost response made the return undiscoverable");
    };
    allowance.shut(Some(added), Fast).unwrap();
    let done = source.finish_return(pending, Slow).unwrap();
    let receipt = receipt.take().unwrap();
    assert_eq!(receipt.id(), pending);
    assert_eq!(done.physical, pages(0, 3));
    let capacity = capacity + 4 * PAGE;
    assert_eq!(
        inspect_budget(source).map(|(total, unassigned)| (total.get(), unassigned.get())),
        Ok((capacity, capacity))
    );
    allowance.reclaim(Slow).ok().unwrap();
    assert_eq!(receipt.finish(Fast), Ok(done));
    receipt.acknowledge().unwrap();
    assert_eq!(receipt.finish(Fast), Err(Error::Stale));
    boot::write("PASS allocator source=1 negative=1 delayed=1 return=1\n");
}
