//! Settle one original boot scratch Allocation on the resident dispatcher stack.

use core::{
    cell::RefCell,
    mem::{align_of, size_of},
};

use kernel::{
    basis::pend::{Handle, Initial as InitialPend, PendSpace, Revoker},
    entry::boot::Boot,
    metal::{
        memory::allocator::treaty::{
            Allocation, AllocationConstruction, Allocator, Allowance, Candidate, Config, Error,
            Initial, LookupObservation, Pending, RamPurpose, Request, ReturnReceipt, Returned,
            Search,
        },
        platform::boot::treaty::write,
        sync::treaty::TurnMode::Fast,
    },
};
use limen_treaty::model::{Alignment, ByteLength, PhysicalRange};

use super::memory::{extent, mapped, retirement};

const PAGE: u64 = 4096;
const BASE: u64 = 0x8440_0000;
const CONFIG: Config = Config {
    granule: ByteLength::new(PAGE),
    maximum: ByteLength::new(PAGE),
    regions: 1,
    grains: 1,
    roots: 1,
    allocations: 1,
    returns: 2,
    allowances: 1,
    edits: 2,
};

static PEND: PendSpace = PendSpace::new();
static REPLACEMENT: AllocationConstruction = AllocationConstruction::new();
static OWNER: Scratch = Scratch(RefCell::new(State {
    allocation: None,
    allowance: None,
    spending: None,
    closing: None,
    receipt: None,
}));

pub(super) struct Scratch(RefCell<State>);

struct State {
    allocation: Option<Allocation>,
    allowance: Option<Allowance>,
    spending: Option<Handle>,
    closing: Option<Revoker>,
    receipt: Option<ReturnReceipt>,
}

// Only this guest's Major accesses the owner. Its early borrow ends before
// the no-return stack switch. Neither the Trap handler nor any Minor accesses it.
unsafe impl Sync for Scratch {}

fn candidate(
    source: &'static Allocator,
    physical: PhysicalRange,
    observations: &mut [LookupObservation; 1],
) -> Option<Candidate> {
    let request = Request::new(
        ByteLength::new(PAGE),
        Alignment::new(PAGE).unwrap(),
        physical,
        None,
        RamPurpose::Metadata,
    )
    .unwrap();
    let mut lookup = source.lookup(request, None, observations).unwrap();
    loop {
        match lookup.step().unwrap() {
            Search::Pending => {}
            Search::Candidate(candidate) => return Some(candidate),
            Search::NoRange => return None,
        }
    }
}

fn observe_initial_occupation(source: &'static Allocator, allocation: &Allocation) {
    let physical = allocation.physical();
    // This is the actual owner of this mapping and only this lexical
    // operation borrows it. RISC-V Bare supplies the ordinary RAM identity map.
    let mut mapping = unsafe { mapped(physical) };
    let bytes = mapping.bytes();
    type Observations = [LookupObservation; 1];
    assert!(bytes.len() >= size_of::<Observations>());
    let pointer = bytes.as_mut_ptr().cast::<Observations>();
    assert!(pointer.addr().is_multiple_of(align_of::<Observations>()));
    let observations = unsafe {
        pointer.write([LookupObservation::EMPTY]);
        &mut *pointer
    };
    assert!(candidate(source, physical, observations).is_none());
}

pub(super) fn prepare(boot: &Boot<'_>) -> &'static Scratch {
    let physical = extent(BASE + PAGE, BASE + 2 * PAGE);
    let evidence = retirement(boot, extent(BASE, BASE + 2 * PAGE));
    let mut geometry = [None; 1];
    let mut regions = boot
        .regions(ByteLength::new(PAGE), Some(&evidence), &mut geometry)
        .unwrap();
    let mut root = Some(unsafe { mapped(extent(BASE, BASE + PAGE)) });
    let mut owner = OWNER.0.borrow_mut();
    let State {
        allocation,
        allowance,
        spending,
        closing,
        ..
    } = &mut *owner;
    let source = unsafe {
        regions.allocator(
            &mut root,
            CONFIG,
            PAGE,
            allowance,
            &mut [Initial::new(physical, allocation)],
        )
    }
    .unwrap()
    .unwrap();
    assert!(root.is_none());
    observe_initial_occupation(source, allocation.as_ref().unwrap());
    unsafe {
        PEND.publish_initial(
            allowance.as_ref().unwrap(),
            ByteLength::new(PAGE),
            Alignment::new(PAGE).unwrap(),
            InitialPend { spending, closing },
            Fast,
        )
    }
    .unwrap();
    &OWNER
}

impl Scratch {
    pub(super) fn settle(&self) {
        let mut owner = self.0.borrow_mut();
        let allowance = owner.allowance.take().unwrap();
        let source = allowance.source();
        let allocation = owner.allocation.take().unwrap();
        let original = allocation.id();
        let physical = allocation.physical();
        let region = allocation.region();
        let mut observations = [LookupObservation::EMPTY; 1];
        assert!(candidate(source, physical, &mut observations).is_none());

        // The old lookup and its exclusive scratch borrow ended on the
        // Major stack. There are no other users or device effects. In Bare
        // mode the physical RAM has no translated access to retire.
        unsafe { allocation.retire().begin_return(&mut owner.receipt) }
            .ok()
            .unwrap();
        let receipt = owner.receipt.as_ref().unwrap();
        assert_eq!(
            source.pending(region, &mut 0, 2),
            Ok(Pending::Found(receipt.id()))
        );
        assert!(candidate(source, physical, &mut observations).is_none());
        let returned = Returned {
            allocation: original,
            physical,
        };
        assert_eq!(receipt.finish(Fast), Ok(returned));

        let replacement = candidate(source, physical, &mut observations).unwrap();
        assert_eq!(replacement.physical(), physical);
        let spending = owner.spending.take().unwrap();
        let replacement_id = spending.reserve(replacement, &REPLACEMENT, Fast).unwrap();
        assert_ne!(replacement_id, original);
        assert_eq!(spending.issue(&REPLACEMENT, Fast), Ok(replacement_id));
        unsafe { REPLACEMENT.take(&mut owner.allocation) }.unwrap();
        let replacement = owner.allocation.take().unwrap();
        assert_eq!(replacement.id(), replacement_id);
        assert_eq!(replacement.region(), region);
        assert_eq!(replacement.physical(), physical);
        let receipt = owner.receipt.take().unwrap();
        // With one allocation slot, the replacement reused its physical
        // range and slot. The retained history still describes the old turn.
        assert_eq!(receipt.finish(Fast), Ok(returned));
        receipt.acknowledge().unwrap();
        assert_eq!(receipt.finish(Fast), Err(Error::Stale));

        // The replacement has had no mapping or other consumer.
        unsafe { replacement.retire().begin_return(&mut owner.receipt) }
            .ok()
            .unwrap();
        let receipt = owner.receipt.take().unwrap();
        assert_eq!(
            receipt.finish(Fast),
            Ok(Returned {
                allocation: replacement_id,
                physical
            }),
        );
        receipt.acknowledge().unwrap();
        owner.closing.take().unwrap().revoke().unwrap();
        spending.release().unwrap();
        allowance.shut(Some(region), Fast).unwrap();
        allowance.reclaim(Fast).ok().unwrap();
        write(
            "PASS boot-scratch runtime-stack=1 retired=1 returned=1 same-range=1 new-generation=1\n",
        );
    }
}
