//! Build an owned Sv39 successor of the real Bare boot environment.
//! The guest maps only its retained image, qualified RAM and original DTB.

use core::{arch::asm, mem::MaybeUninit, ptr};

use kernel::{
    basis::{
        cell::CellSlot,
        pend::{Handle, Initial, PendSpace},
    },
    entry::boot::Boot,
    metal::{
        memory::{
            allocator::treaty::{
                AllocationConstruction, AllocationId, Allowance, Config, LookupObservation,
                MappedStorage, RamPurpose, Request, Search,
            },
            mmu::{
                riscv64::internal::{
                    context::Context,
                    description::{MaximumMode, RiscvMmu},
                    format::Format,
                    installation::admit_context,
                },
                treaty::{
                    LeafAccess, Memory, TableBranch, TableError, TableFormat, TableProgress, TableId, TableStore,
                },
            },
        },
        sync::treaty::TurnMode::Fast,
    },
};
use limen_treaty::model::{Alignment, ByteLength, InternalAddress, PhysicalAddress, PhysicalRange};

#[path = "../../../../../entry/boot/material/memory.rs"]
mod memory;
use memory::{extent, mapped, retirement};

const PAGE: u64 = 4096;
const BASE: u64 = 0x8460_0000;
const PAGES: usize = 32;
const BACKING: usize = 8;
const CAPACITY: usize = 16;
const FORMAT: Format = Format::Sv39;
const ALIAS: u64 = 0xffff_ffc0_0000_0000;

unsafe extern "C" {
    static limen_test_text_start: u8;
    static limen_test_read_only: u8;
    static limen_test_writable: u8;
}

#[repr(C, align(4096))]
struct Witness([u64; 512]);

static WITNESS: Witness = Witness([0x51ab_0dcd_1234_abcd; 512]);
static mut WRITABLE: u64 = 0x1234;
static SPENDING: PendSpace = PendSpace::for_page_tables();
static CONSTRUCTION: [AllocationConstruction; CAPACITY] =
    [const { AllocationConstruction::new() }; CAPACITY];
static CELLS: [CellSlot; CAPACITY] = [const { CellSlot::new() }; CAPACITY];
static mut BRANCHES: [[TableBranch; CAPACITY]; CAPACITY] =
    [const { [const { TableBranch::EMPTY }; CAPACITY] }; CAPACITY];
static mut TABLES: [MaybeUninit<TableStore>; CAPACITY] =
    [const { MaybeUninit::uninit() }; CAPACITY];

#[derive(Clone, Copy)]
struct Node {
    level: usize,
    prefix: u64,
    store: &'static TableStore,
    id: TableId,
    allocation: AllocationId,
}

struct Tree<'a> {
    allowance: &'a Allowance,
    handle: Handle,
    nodes: [Option<Node>; CAPACITY],
    count: usize,
}

impl Tree<'_> {
    fn prefix(level: usize, address: u64) -> u64 {
        if level == FORMAT.levels() - 1 {
            0
        } else {
            address >> (12 + 9 * (level + 1))
        }
    }

    fn find(&self, level: usize, address: u64) -> Option<Node> {
        self.nodes
            .iter()
            .flatten()
            .copied()
            .find(|node| node.level == level && node.prefix == Self::prefix(level, address))
    }

    fn node(&mut self, level: usize, address: u64) -> Node {
        if let Some(node) = self.find(level, address) {
            return node;
        }
        let slot = self.count;
        assert!(slot < CAPACITY);
        let physical = BASE + (BACKING + slot) as u64 * PAGE;
        let range = extent(physical, physical + PAGE);
        let request = Request::new(
            ByteLength::new(PAGE),
            Alignment::new(PAGE).unwrap(),
            range,
            None,
            RamPurpose::PageTable,
        )
        .unwrap();
        let mut observations = [LookupObservation::EMPTY; 1];
        let mut lookup = self
            .allowance
            .source()
            .lookup(request, None, &mut observations)
            .unwrap();
        let candidate = loop {
            match lookup.step().unwrap() {
                Search::Candidate(value) => break value,
                Search::Pending => {}
                Search::NoRange => panic!("prepaid owned table page"),
            }
        };
        assert_eq!(candidate.physical(), range);
        let construction = &CONSTRUCTION[slot];
        let allocation = self.handle.reserve(candidate, construction, Fast).unwrap();
        assert_eq!(self.handle.issue(construction, Fast), Ok(allocation));
        let mut receiver = None;
        // This nonreturning test fixture is the original receiving owner
        // until the stable table accepts the actual allocation.
        unsafe { construction.take(&mut receiver) }.unwrap();
        let material = unsafe { MappedStorage::prepare(receiver.take().unwrap(), mapped(range)) }
            .ok()
            .unwrap();
        // Each slot is constructed once. Never borrow the enclosing arrays
        // again after publishing shared references to earlier elements.
        let table = unsafe {
            &mut *(&raw mut TABLES)
                .cast::<MaybeUninit<TableStore>>()
                .add(slot)
        };
        let branches = unsafe {
            &mut *(&raw mut BRANCHES)
                .cast::<[TableBranch; CAPACITY]>()
                .add(slot)
        };
        let links = if level == 0 {
            &mut branches[..0]
        } else {
            &mut branches[..]
        };
        let store =
            table.write(TableStore::for_level(TableFormat::Riscv64(FORMAT), level, links).unwrap());
        let id = store
            .publish_cell(&CELLS[slot], self.handle, material, Fast)
            .ok()
            .unwrap();
        store.prepare(id, Fast).unwrap();
        store.accept(id, Fast).unwrap();
        store.initialize(id, Fast).unwrap();
        assert_eq!(store.locate(allocation), Ok(Some(id)));
        let node = Node {
            level,
            prefix: Self::prefix(level, address),
            store,
            id,
            allocation,
        };
        self.nodes[slot] = Some(node);
        self.count += 1;
        node
    }

    fn map(&mut self, address: u64, physical: PhysicalAddress, access: LeafAccess) {
        let leaf = self.node(0, address);
        self.node(1, address);
        self.node(2, address);
        let index = FORMAT.index(InternalAddress::new(address), 0).unwrap();
        // The guest owns the exact fixed-image, retained description or
        // qualified RAM extent that this particular leaf is allowed to reach.
        unsafe {
            leaf.store
                .map_leaf(leaf.id, index, physical, access, Memory::Normal, Fast)
        }
        .unwrap();
        assert_eq!(
            leaf.store.descriptor(leaf.id, index, Fast).unwrap(),
            FORMAT.leaf(physical, 0, access).unwrap()
        );
    }

    fn identity(&mut self, range: PhysicalRange, access: LeafAccess) {
        assert!(range.start().get().is_multiple_of(PAGE));
        assert!(range.end().get().is_multiple_of(PAGE));
        for address in (range.start().get()..range.end().get()).step_by(PAGE as usize) {
            self.map(address, PhysicalAddress::new(address), access);
        }
    }

    fn parent(&self, child: Node) -> (Node, usize) {
        let address = child.prefix << (12 + 9 * (child.level + 1));
        let parent = self
            .find(child.level + 1, address)
            .expect("original prepared parent");
        let index = FORMAT
            .index(InternalAddress::new(address), parent.level)
            .unwrap();
        (parent, index)
    }

    fn link(&self) {
        for level in 0..FORMAT.levels() - 1 {
            for child in self
                .nodes
                .iter()
                .flatten()
                .copied()
                .filter(|node| node.level == level)
            {
                let (parent, index) = self.parent(child);
                parent
                    .store
                    .link_child(parent.id, index, child.store, child.id, Fast)
                    .unwrap();
            }
        }
    }


}

pub fn check(boot: &Boot<'_>) {
    assert_eq!(Context::current(), Some(Context::BARE));
    let cpu = boot
        .riscv_cpus()
        .cpu(boot.processor() as u64)
        .unwrap()
        .unwrap();
    let mmu = RiscvMmu::describe(&cpu).unwrap().unwrap();
    assert!(matches!(mmu.maximum(), MaximumMode::Paged(mode) if mode >= FORMAT));
    let mut control = unsafe { admit_context(mmu) }.unwrap();

    let range = extent(BASE, BASE + PAGES as u64 * PAGE);
    let evidence = retirement(boot, range);
    let mut storage = [None; 1];
    let mut regions = boot
        .regions(ByteLength::new(PAGE), Some(&evidence), &mut storage)
        .unwrap();
    assert!(regions.entries().map(|region| region.physical).eq([range]));
    let backing = extent(BASE, BASE + BACKING as u64 * PAGE);
    let mut allowance = None;
    let source = unsafe {
        regions.allocator(
            &mut Some(mapped(backing)),
            Config {
                granule: ByteLength::new(PAGE),
                maximum: range.length(),
                regions: 1,
                grains: PAGES,
                roots: 8,
                allocations: CAPACITY,
                returns: CAPACITY,
                allowances: 1,
                edits: 128,
            },
            CAPACITY as u64 * PAGE,
            &mut allowance,
            &mut [],
        )
    }
    .unwrap()
    .unwrap();
    let allowance = allowance.as_ref().unwrap();
    assert!(ptr::eq(source, allowance.source()));
    let mut spending = None;
    let mut closing = None;
    unsafe {
        SPENDING.publish_initial(
            allowance,
            ByteLength::new(PAGE),
            Alignment::new(PAGE).unwrap(),
            Initial {
                spending: &mut spending,
                closing: &mut closing,
            },
            Fast,
        )
    }
    .unwrap();
    let handle = spending.unwrap();
    let original = allowance.balance().unwrap();

    let mut tree = Tree {
        allowance,
        handle,
        nodes: [None; CAPACITY],
        count: 0,
    };
    let image = boot.image();
    let text = (&raw const limen_test_text_start).addr() as u64;
    let read_only = (&raw const limen_test_read_only).addr() as u64;
    let writable = (&raw const limen_test_writable).addr() as u64;
    assert_eq!(text, image.start().get());
    assert!(text < read_only && read_only < writable && writable < image.end().get());
    tree.identity(extent(text, read_only), LeafAccess::ReadExecute);
    tree.identity(extent(read_only, writable), LeafAccess::ReadOnly);
    tree.identity(extent(writable, image.end().get()), LeafAccess::ReadWrite);
    tree.identity(range, LeafAccess::ReadWrite);

    // The original firmware description stays readable through the switch.
    // Its whole pages belong to the retained, independently described RAM.
    let dtb = boot.device_tree().expect("retained DTB");
    let first = PhysicalAddress::new(dtb.start as u64).align_down(Alignment::new(PAGE).unwrap());
    let end = PhysicalAddress::new(dtb.end as u64)
        .align_up(Alignment::new(PAGE).unwrap())
        .unwrap();
    let retained = PhysicalRange::new(first, end).unwrap();
    assert!(
        boot.dtb_memory()
            .unwrap()
            .ram()
            .any(|ram| ram.covers(retained))
    );
    assert!(!image.overlaps(retained) && !range.overlaps(retained));
    tree.identity(retained, LeafAccess::ReadOnly);

    let witness = ptr::from_ref(&WITNESS).addr() as u64;
    assert!(extent(read_only, writable).covers(extent(witness, witness + PAGE)));
    tree.map(ALIAS, PhysicalAddress::new(witness), LeafAccess::ReadOnly);
    tree.link();
    let root = tree.find(2, text).unwrap();
    let physical = root.store.cell(root.id).unwrap().1.start();
    let charged = allowance.balance().unwrap();
    assert_eq!(charged.occupied().get(), tree.count as u64 * PAGE);

    let mut active = unsafe { root.store.prepare_local(root.id, &mut control, Fast) }.unwrap();
    let mut retired = None;
    assert_eq!(
        unsafe { root.store.retire_private(root.id, &mut retired, Fast) },
        Err(TableError::State)
    );
    assert!(retired.is_none());
    unsafe { active.install() }.unwrap();
    assert_eq!(Context::current(), Context::paged(FORMAT, physical));
    let value: u64;
    unsafe { asm!("ld {}, 0({})", out(reg) value, in(reg) ALIAS, options(nostack, readonly)) };
    assert_eq!(value, WITNESS.0[0]);
    unsafe {
        (&raw mut WRITABLE).write_volatile(0x5678);
        assert_eq!((&raw const WRITABLE).read_volatile(), 0x5678);
    }
    let index = FORMAT.index(InternalAddress::new(text), 2).unwrap();
    assert_eq!(
        root.store.unlink_child(root.id, index, Fast),
        Err(TableError::State)
    );
    assert_eq!(active.finish(Fast), Err(TableError::State));
    assert_eq!(allowance.balance(), Ok(charged));

    unsafe { active.restore() }.unwrap();
    assert_eq!(Context::current(), Some(Context::BARE));
    // More than one real subtree is owned by this root. Each refund
    // must settle before its detached original branch can disappear.
    for _ in 0..(CAPACITY * 6 + 2) {
        if unsafe { active.advance_retirement(Fast) }.unwrap() == TableProgress::Complete {
            break;
        }
    }
    assert_eq!(unsafe { active.advance_retirement(Fast) }, Ok(TableProgress::Complete));
    drop(active);
    assert_eq!(control.current().unwrap().root(), None);
    assert_eq!(unsafe { (&raw const WRITABLE).read_volatile() }, 0x5678);
    for node in tree.nodes.iter().flatten() {
        assert_eq!(node.store.locate(node.allocation), Ok(None));
    }
    assert_eq!(allowance.balance(), Ok(original));
    closing.as_ref().unwrap().revoke().unwrap();
    handle.release().unwrap();
    kernel::metal::platform::boot::treaty::write(
        "PASS owned sv39 image-rx-ro-rw=1 ram=1 dtb=1 native-root=1 alias=1 restore=1 refund=1\n",
    );
}
