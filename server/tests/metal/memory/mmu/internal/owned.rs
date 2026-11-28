//! Common material for the two real four-level owned-root UEFI scenarios.

use core::{mem::MaybeUninit, ptr};

use kernel::{
    basis::{
        cell::CellSlot,
        pend::{self, Initial, PendSpace, Revoker},
    },
    entry::boot::{Boot, Retirement},
    metal::{
        memory::{
            allocator::treaty::{
                Allocation, AllocationConstruction, AllocationId, Allowance, Balance, Config,
                LookupObservation, MappedStorage, RamPurpose, Request, Retired, ReturnReceipt,
                Search,
            },
            mmu::treaty::{TableBranch, TableFormat, TableId, TableStore},
        },
        sync::treaty::TurnMode::Fast,
    },
};
use limen_treaty::model::{Alignment, ByteLength, PhysicalRange};

use super::super::super::memory::{extent, mapped};

pub(super) const PAGES: usize = 8;
const PAGE: u64 = 4096;
const LEVELS: usize = 4;

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

unsafe fn make_table(
    store: &'static TableStore,
    allowance: &Allowance,
    handle: pend::Handle,
    level: usize,
    range: PhysicalRange,
) -> (TableId, AllocationId) {
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

#[derive(Clone, Copy)]
pub(super) struct Node {
    pub(super) store: &'static TableStore,
    pub(super) id: TableId,
    allocation: AllocationId,
}

pub(super) struct Tree {
    pub(super) nodes: [Node; LEVELS],
    pub(super) allowance: &'static Allowance,
    handle: pend::Handle,
    closing: &'static Revoker,
    balance: Balance,
}

impl Tree {
    /// # Safety
    /// The fixture exclusively owns the original qualified RAM and keeps
    /// its real writable identity view throughout construction and use.
    pub(super) unsafe fn prepare(
        boot: &Boot<'_>,
        range: PhysicalRange,
        evidence: &Retirement,
        format: TableFormat,
    ) -> Self {
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

        Self {
            nodes: nodes.map(|(store, id, allocation)| Node {
                store,
                id,
                allocation,
            }),
            allowance,
            handle,
            closing: closing.as_ref().unwrap(),
            balance,
        }
    }

    /// Indexes from the bottom branch to the root branch.
    pub(super) fn link(&self, indexes: [usize; LEVELS - 1]) {
        for (level, index) in indexes.into_iter().enumerate() {
            let child = self.nodes[level];
            let parent = self.nodes[level + 1];
            parent
                .store
                .link_child(parent.id, index, child.store, child.id, Fast)
                .unwrap();
        }
        assert_eq!(
            self.allowance.balance().unwrap().occupied().get(),
            PAGE * LEVELS as u64
        );
    }

    /// # Safety
    /// Actual native retirement has settled, and the private tree has no
    /// possible hardware path or other owner. Return only its original pages.
    pub(super) unsafe fn finish(self, indexes: [usize; LEVELS - 1]) {
        for (level, index) in indexes.into_iter().enumerate().rev() {
            let child = self.nodes[level];
            let parent = self.nodes[level + 1];
            assert_eq!(
                parent.store.unlink_child(parent.id, index, Fast),
                Ok(child.id)
            );
        }
        let retired = unsafe { &mut *(&raw mut RETIRED) };
        let receipt = unsafe { &mut *(&raw mut RETURNED) };
        for Node {
            store,
            id,
            allocation,
        } in self.nodes.into_iter().rev()
        {
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
        assert_eq!(self.allowance.balance(), Ok(self.balance));
        self.closing.revoke().unwrap();
        self.handle.release().unwrap();
    }
}
