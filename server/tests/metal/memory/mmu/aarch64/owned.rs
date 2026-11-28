//! Own, activate, and retire the first complete EL1 upper table tree.

use core::{arch::asm, mem::MaybeUninit, ptr};

use kernel::{
    basis::{
        cell::CellSlot,
        pend::{self, Initial, PendSpace, Revoker},
    },
    entry::boot::{Boot, Retirement},
    metal::{
        memory::{
            allocator::treaty::{
                Allocation, AllocationConstruction, AllocationId, Allowance, Config,
                LookupObservation, MappedStorage, RamPurpose, Request, Retired, ReturnReceipt,
                Search,
            },
            mmu::{
                aarch64::internal::{context::Controls, installation::admit_inactive_upper},
                treaty::{LeafAccess, Memory, TableBranch, TableFormat, TableId, TableStore},
            },
        },
        sync::treaty::TurnMode::Fast,
    },
};
use limen_treaty::model::{Alignment, ByteLength, PhysicalAddress, PhysicalRange};

use super::super::memory::{extent, mapped};

pub(super) const PAGES: usize = 8;
const PAGE: u64 = 4096;
const LEVELS: usize = 4;
const UPPER: u64 = 0xffff_0000_0000_0000;
const OUTPUT: u64 = 0x0000_ffff_ffff_f000;
const MEMORY_ATTRIBUTES: u64 = 0xff00_0000_0000_0180;

#[repr(C, align(4096))]
struct Witness([u64; 512]);

static WITNESS: Witness = Witness([0x50f1_0ace_d000_4321; 512]);
static SPENDING: PendSpace = PendSpace::for_page_tables();
static CONSTRUCTION: [AllocationConstruction; LEVELS] =
    [const { AllocationConstruction::new() }; LEVELS];
static CELLS: [CellSlot; LEVELS] = [const { CellSlot::new() }; LEVELS];
static mut BRANCHES: [[TableBranch; 1]; LEVELS] = [const { [TableBranch::EMPTY] }; LEVELS];
static mut TABLES: [MaybeUninit<TableStore>; LEVELS] = [const { MaybeUninit::uninit() }; LEVELS];
static mut ALLOWANCE: Option<Allowance> = None;
static mut HANDLE: Option<pend::Handle> = None;
static mut REVOCATION: Option<Revoker> = None;
static mut ALLOCATION: Option<Allocation> = None;
static mut RETIRED: Option<Retired> = None;
static mut RETURNED: Option<ReturnReceipt> = None;

fn translation(address: u64) -> u64 {
    let result: u64;
    unsafe {
        asm!("at s1e1r, {}", "isb", "mrs {}, par_el1",
            in(reg) address, out(reg) result, options(nostack));
    }
    result
}

fn write_translation(address: u64) -> u64 {
    let result: u64;
    unsafe {
        asm!("at s1e1w, {}", "isb", "mrs {}, par_el1",
            in(reg) address, out(reg) result, options(nostack));
    }
    result
}

fn verify_lower_writable(range: PhysicalRange) {
    let start = range.start().get();
    let end = range.end().get();
    for page in (start..end).step_by(PAGE as usize) {
        let first = write_translation(page);
        let last = write_translation(page + PAGE - 1);
        assert_eq!(first & (OUTPUT | 1), page);
        assert_eq!(last & (OUTPUT | 1), page);
        assert_eq!(first & MEMORY_ATTRIBUTES, last & MEMORY_ATTRIBUTES);
    }
}

unsafe fn make_table(
    store: &'static TableStore,
    allowance: &Allowance,
    handle: pend::Handle,
    level: usize,
    range: PhysicalRange,
) -> (TableId, AllocationId) {
    verify_lower_writable(range);
    let request = Request::new(
        ByteLength::new(PAGE),
        Alignment::new(PAGE).unwrap(),
        range,
        None,
        RamPurpose::PageTable,
    )
    .unwrap();
    let mut observations = [LookupObservation::EMPTY; 1];
    let mut lookup = allowance
        .source()
        .lookup(request, None, &mut observations)
        .unwrap();
    let candidate = loop {
        match lookup.step().unwrap() {
            Search::Candidate(value) => break value,
            Search::Pending => {}
            Search::NoRange => panic!("owned table page is available"),
        }
    };
    assert_eq!(candidate.physical(), range);
    let construction = &CONSTRUCTION[level];
    let allocation_id = handle.reserve(candidate, construction, Fast).unwrap();
    assert_eq!(handle.issue(construction, Fast), Ok(allocation_id));
    let original = unsafe { &mut *(&raw mut ALLOCATION) };
    unsafe { construction.take(original) }.unwrap();
    // Exact Allocation and its independently retained lower CPU mapping.
    let material = unsafe { MappedStorage::prepare(original.take().unwrap(), mapped(range)) }
        .ok()
        .unwrap();
    let id = store
        .publish_cell(&CELLS[level], handle, material, Fast)
        .ok()
        .unwrap();
    store.prepare(id, Fast).unwrap();
    store.accept(id, Fast).unwrap();
    store.initialize(id, Fast).unwrap();
    assert_eq!(store.locate(allocation_id), Ok(Some(id)));
    (id, allocation_id)
}

/// # Safety
/// The fixture owns the original unexposed grant and its writable lower
/// mapping. The lower walk remains resident and is the only live walk.
pub(super) unsafe fn check(boot: &Boot<'_>, range: PhysicalRange, evidence: &Retirement) {
    let start = range.start().get();
    let mut scratch = [None; 1];
    let mut regions = boot
        .regions(ByteLength::new(PAGE), Some(evidence), &mut scratch)
        .unwrap();
    assert!(regions.entries().map(|entry| entry.physical).eq([range]));
    let config = Config {
        granule: ByteLength::new(PAGE),
        maximum: ByteLength::new(range.length().get()),
        regions: 1,
        grains: PAGES,
        roots: 4,
        allocations: LEVELS,
        returns: LEVELS,
        allowances: 1,
        edits: 64,
    };
    let backing = extent(start, start + 2 * PAGE);
    verify_lower_writable(backing);
    let allowance = unsafe { &mut *(&raw mut ALLOWANCE) };
    let source = unsafe {
        regions.allocator(
            &mut Some(mapped(backing)),
            config,
            4 * PAGE,
            allowance,
            &mut [],
        )
    }
    .unwrap()
    .unwrap();
    let allowance = allowance.as_ref().unwrap();
    assert!(ptr::eq(source, allowance.source()));
    let spending = unsafe { &mut *(&raw mut HANDLE) };
    let closing = unsafe { &mut *(&raw mut REVOCATION) };
    let granule = Alignment::new(PAGE).unwrap();
    unsafe {
        SPENDING.publish_initial(
            allowance,
            ByteLength::new(PAGE),
            granule,
            Initial { spending, closing },
            Fast,
        )
    }
    .unwrap();
    let handle = spending.unwrap();
    let balance = allowance.balance().unwrap();

    let original = unsafe { Controls::current() };
    let (mut control, profile) = unsafe { admit_inactive_upper(boot.processor()) }.unwrap();
    let format = TableFormat::Aarch64(profile);
    let tables = unsafe { &mut *(&raw mut TABLES) };
    let branches = unsafe { &mut *(&raw mut BRANCHES) };
    let mut material = tables.iter_mut().zip(branches).enumerate();
    let nodes: [(&'static TableStore, TableId, AllocationId); LEVELS] =
        core::array::from_fn(|_| {
            let (level, (table, links)) = material.next().expect("one real table page");
            let links = &mut links[..usize::from(level != 0)];
            let store: &'static TableStore =
                table.write(TableStore::for_level(format, level, links).unwrap());
            let physical = start + (level as u64 + 2) * PAGE;
            let (id, allocation) = unsafe {
                make_table(
                    store,
                    allowance,
                    handle,
                    level,
                    extent(physical, physical + PAGE),
                )
            };
            (store, id, allocation)
        });
    let [
        (leaf, leaf_id, leaf_allocation),
        (low, low_id, low_allocation),
        (high, high_id, high_allocation),
        (root, root_id, root_allocation),
    ] = nodes;

    let lower = ptr::from_ref(&WITNESS).addr() as u64;
    let original_witness = translation(lower);
    assert_eq!(original_witness & 1, 0);
    let witness = original_witness & OUTPUT;
    assert!(boot.image().covers(extent(witness, witness + PAGE)));
    assert_ne!(translation(UPPER) & 1, 0);
    unsafe {
        leaf.map_leaf(
            leaf_id,
            0,
            PhysicalAddress::new(witness),
            LeafAccess::ReadOnly,
            Memory::Normal,
            Fast,
        )
    }
    .unwrap();
    low.link_child(low_id, 0, leaf, leaf_id, Fast).unwrap();
    high.link_child(high_id, 0, low, low_id, Fast).unwrap();
    root.link_child(root_id, 0, high, high_id, Fast).unwrap();
    let charged = allowance.balance().unwrap();
    assert_eq!(charged.occupied().get(), PAGE * LEVELS as u64);

    let root_address = root.cell(root_id).unwrap().1.start().get();
    let mut active = unsafe { root.prepare_local(root_id, &mut control, Fast) }.unwrap();
    let retired = unsafe { &mut *(&raw mut RETIRED) };
    assert!(unsafe { root.retire_private(root_id, retired, Fast) }.is_err() && retired.is_none());
    unsafe { active.install() }.unwrap();
    let current = unsafe { Controls::current() };
    assert_eq!(current.ttbr1, root_address);
    assert_eq!(current.ttbr0, original.ttbr0);
    const UPPER_FIELDS: u64 = 0xffbf << 16;
    assert_eq!(current.tcr & !UPPER_FIELDS, original.tcr & !UPPER_FIELDS);
    assert_eq!(current.mair, original.mair);
    assert_eq!(current.sctlr, original.sctlr);
    assert_eq!(translation(lower), original_witness);
    let on_upper = translation(UPPER);
    assert_eq!(on_upper & (OUTPUT | 1), witness);
    assert_eq!(
        on_upper & MEMORY_ATTRIBUTES,
        original_witness & MEMORY_ATTRIBUTES
    );
    assert_ne!(write_translation(UPPER) & 1, 0);
    let value: u64;
    // Raw register load avoids fabricating a second Rust reference. The
    // descriptor and actual native AT prove the original, immutable page.
    unsafe { asm!("ldr {}, [{}]", out(reg) value, in(reg) UPPER, options(nostack, readonly)) };
    assert_eq!(value, WITNESS.0[0]);
    assert!(root.unlink_child(root_id, 0, Fast).is_err());
    assert!(active.finish(Fast).is_err());
    assert_eq!(allowance.balance(), Ok(charged));

    unsafe { active.restore() }.unwrap();
    assert_eq!(unsafe { Controls::current() }, original);
    assert_ne!(translation(UPPER) & 1, 0);
    active.finish(Fast).unwrap();
    drop(active);
    assert_eq!(control.current().unwrap().root(), None);

    assert_eq!(root.unlink_child(root_id, 0, Fast), Ok(high_id));
    assert_eq!(high.unlink_child(high_id, 0, Fast), Ok(low_id));
    assert_eq!(low.unlink_child(low_id, 0, Fast), Ok(leaf_id));
    let receipt = unsafe { &mut *(&raw mut RETURNED) };
    for (store, id, allocation) in [
        (root, root_id, root_allocation),
        (high, high_id, high_allocation),
        (low, low_id, low_allocation),
        (leaf, leaf_id, leaf_allocation),
    ] {
        assert_eq!(store.locate(allocation), Ok(Some(id)));
        unsafe { store.retire_private(id, retired, Fast) }.unwrap();
        unsafe { retired.take().unwrap().begin_return(receipt) }
            .ok()
            .unwrap();
        let returned = receipt.take().unwrap();
        assert_eq!(returned.finish(Fast).unwrap().allocation, allocation);
        returned.acknowledge().unwrap();
        assert_eq!(store.locate(allocation), Ok(None));
    }
    assert_eq!(allowance.balance(), Ok(balance));
    closing.as_ref().unwrap().revoke().unwrap();
    handle.release().unwrap();
    kernel::metal::platform::boot::treaty::write(
        "PASS owned upper tables=4 active=1 access=1 original=1 retire=1 refund=4\n",
    );
}
