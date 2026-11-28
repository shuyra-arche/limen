//! Local Internal Nest lifetime joined to actual Unit/Room/native continuation.

use core::{
    arch::asm,
    cell::RefCell,
    mem::MaybeUninit,
    ptr::{self, NonNull},
    sync::atomic::{AtomicUsize, Ordering},
};
use kernel::{
    basis::{
        execution::{Current, Error, Execution, KernelBinding, RuntimeEntry},
        nest::{self, LocalNest, Nest, NestId, Phase, Progress},
        room::{Occupancy, Participants},
        unit::{End, Lifecycle, Stacks, UnitId, UnitRecord},
    },
    entry::boot::{Boot, Condition, Retirement},
    metal::{
        memory::mmu::{
            treaty::{LeafAccess, LocalContext, Memory, TableError, TableFormat},
            x86_64::internal::{context::Controls, installation::admit_root},
        },
        processor::treaty::Dispatchers,
        sync::treaty::TurnMode::Fast,
    },
};
use limen_treaty::model::PhysicalAddress;
use uefi::Status;

#[path = "../material/memory.rs"]
mod memory;
#[path = "../../../metal/memory/mmu/internal/owned.rs"]
mod tree;
#[path = "../material/window.rs"]
mod window;

use tree::Tree;

static WINDOW: window::Window<{ tree::PAGES }> = window::Window::new();
static NEST: Nest = Nest::new();
static ALIAS: AtomicUsize = AtomicUsize::new(0);
static mut CONTROL: MaybeUninit<LocalContext<'static>> = MaybeUninit::uninit();
static mut STACKS: Stacks = Stacks::new();
static mut RECORDS: [UnitRecord<KernelBinding>; 4] = [const { UnitRecord::EMPTY }; 4];
static mut PARTICIPANTS: Participants<1> = Participants::new();
static mut DISPATCHERS: Dispatchers<Occupancy, 1> = Dispatchers::new();
static mut EXECUTION: MaybeUninit<Execution> = MaybeUninit::uninit();
static mut RUNTIME: MaybeUninit<RuntimeEntry<Initial>> = MaybeUninit::uninit();

#[repr(C, align(4096))]
struct Witness([u64; 512]);

static WITNESS: Witness = Witness([0x528c_4242_ace0_1234; 512]);

pub fn prepare() -> Result<(), Status> {
    WINDOW.prepare()
}

struct Initial {
    owner: RefCell<LocalNest<'static, 'static>>,
    tree: RefCell<Option<Tree>>,
    original: Controls,
    id: NestId,
    unit: UnitId,
    complete: fn() -> !,
}

fn execution() -> &'static Execution {
    unsafe { (&*(&raw const EXECUTION)).assume_init_ref() }
}

fn check_refs(id: NestId, phase: Phase, references: usize) {
    let observation = NEST.observe(id).unwrap();
    assert_eq!(observation.phase, phase);
    assert_eq!(observation.recoverable_refs, references);
}

fn never(_: &mut Current) -> u64 {
    panic!("cancelled dormant Unit ran");
}

fn continuation(current: &mut Current) -> u64 {
    let nest = execution().unit_nest(current.id()).unwrap().unwrap();
    check_refs(nest, Phase::Shutting, 1);
    let alias = ALIAS.load(Ordering::Relaxed);
    let witness: u64;
    unsafe { asm!("mov {}, [{}]", out(reg) witness, in(reg) alias, options(nostack, readonly)) };
    assert_eq!(witness, WITNESS.0[0]);
    let local = core::hint::black_box(42u64);
    let before = ptr::from_ref(&local).addr();
    current.yield_now().unwrap();
    assert_eq!(execution().unit_nest(current.id()), Ok(Some(nest)));
    assert_eq!(ptr::from_ref(&local).addr(), before);
    check_refs(nest, Phase::Shutting, 1);
    core::hint::black_box(local)
}

pub fn check(mut boot: Boot<'_>, complete: fn() -> !) -> ! {
    uefi_fixture::check_handoff(&boot, None);
    let physical = WINDOW.inspect(&boot);
    let mut evidence = Retirement::new(physical);
    unsafe {
        evidence.record(Condition::NoEarlyBorrow);
        evidence.record(Condition::TranslationRetired);
        evidence.record(Condition::DevicesIsolated);
    }

    let original = unsafe { Controls::current() };
    let (control, format) = unsafe { admit_root(boot.processor()) }.unwrap();
    let control = unsafe { (&mut *(&raw mut CONTROL)).write(control) };
    let old = NonNull::new(ptr::with_exposed_provenance_mut::<u64>(original.cr3)).unwrap();
    assert!(boot.image().end().get() <= 1 << 39 && physical.end().get() <= 1 << 39);
    assert_ne!(unsafe { old.as_ptr().read_volatile() } & 1, 0);
    let slot = (1..256)
        .find(|&index| unsafe { old.as_ptr().add(index).read_volatile() } == 0)
        .expect("unused original PML4 slot");
    let tree = unsafe { Tree::prepare(&boot, physical, &evidence, TableFormat::X86_64(format)) };
    let [leaf, _, _, root] = tree.nodes;
    let witness = ptr::from_ref(&WITNESS).addr() as u64;
    assert!(boot.image().covers(memory::extent(witness, witness + 4096)));
    unsafe {
        leaf.store.map_leaf(
            leaf.id,
            0,
            PhysicalAddress::new(witness),
            LeafAccess::ReadOnly,
            Memory::Normal,
            Fast,
        )
    }
    .unwrap();
    tree.link([0, 0, slot]);
    ALIAS.store(slot << 39, Ordering::Relaxed);
    let prepared = unsafe { root.store.prepare_shared(root.id, control, old, 0, Fast) }.unwrap();
    // A prepared root is not a real installed Nest.
    let (mut prepared, failure) = NEST.publish(prepared).err().unwrap();
    assert_eq!(failure, nest::Error::State);
    unsafe { prepared.install() }.unwrap();
    let mut owner = NEST.publish(prepared).ok().unwrap();
    let id = owner.id();
    assert_eq!(NEST.observe(id).unwrap().root, root.id);
    check_refs(id, Phase::Open, 0);

    let mut execution = Execution::new(
        boot.take_major().unwrap(),
        boot.terminal(),
        unsafe { &mut *(&raw mut STACKS) },
        unsafe { &mut *(&raw mut RECORDS) },
        unsafe { &mut *(&raw mut PARTICIPANTS) },
        unsafe { &mut *(&raw mut DISPATCHERS) },
    )
    .map_err(|failure| failure.reason)
    .unwrap();
    execution.bind_nest(&owner).unwrap();
    let dormant = unsafe { execution.create(never) }.unwrap();
    let unit = unsafe { execution.create(continuation) }.unwrap();
    assert_eq!(execution.unit_nest(dormant), Ok(Some(id)));
    assert_eq!(execution.unit_nest(unit), Ok(Some(id)));
    check_refs(id, Phase::Open, 2);
    assert_eq!(unsafe { execution.create(never) }, Err(Error::Capacity));
    // This local root cannot provide a migratable execution profile.
    assert_eq!(
        unsafe { execution.create_migratable(never) },
        Err(Error::Unsupported)
    );
    check_refs(id, Phase::Open, 2);

    owner.begin_delete().unwrap();
    check_refs(id, Phase::Shutting, 2);
    assert_eq!(owner.begin_drain(), Err(nest::Error::Busy));
    assert_eq!(unsafe { owner.advance_retirement() }, Err(nest::Error::Busy));
    assert_eq!(
        unsafe { execution.create(never) },
        Err(Error::Nest(nest::Error::Shut))
    );
    execution.stop_dormant(dormant).unwrap();
    assert_eq!(execution.unit_nest(dormant), Ok(Some(id)));
    check_refs(id, Phase::Shutting, 2);
    execution.finish_end(dormant).unwrap();
    execution.finish_end(dormant).unwrap();
    assert_eq!(execution.unit_nest(dormant), Ok(None));
    assert_eq!(execution.reap(dormant), Ok(End::Cancelled));
    check_refs(id, Phase::Shutting, 1);
    // Existing Dormant references can still Submit after closure.
    execution.submit(unit).unwrap();
    let execution = unsafe { (&mut *(&raw mut EXECUTION)).write(execution) };
    let initial = Initial {
        owner: RefCell::new(owner),
        tree: RefCell::new(Some(tree)),
        original,
        id,
        unit,
        complete,
    };
    let entry = unsafe { (&mut *(&raw mut RUNTIME)).write(RuntimeEntry::new(schedule, initial)) };
    unsafe { execution.run(entry) }.expect("resident dispatcher")
}

fn schedule(initial: &'static Initial) -> ! {
    let execution = execution();
    let mut owner = initial.owner.borrow_mut();
    execution.attach().unwrap();
    let yielded = execution.enter().unwrap().unwrap();
    assert_eq!(yielded.unit(), initial.unit);
    assert_eq!(execution.lifecycle(initial.unit), Ok(Lifecycle::Live));
    assert_eq!(owner.begin_drain(), Err(nest::Error::Busy));
    assert_eq!(execution.pending_leave(), Ok(Some(yielded)));
    execution.vacate(yielded).unwrap();
    check_refs(initial.id, Phase::Shutting, 1);

    let ended = execution.enter().unwrap().unwrap();
    assert_eq!(ended.unit(), initial.unit);
    assert_ne!(yielded, ended);
    assert_eq!(execution.lifecycle(initial.unit), Ok(Lifecycle::Ending));
    assert_eq!(execution.finish_end(initial.unit), Err(Error::Busy));
    assert_eq!(owner.begin_drain(), Err(nest::Error::Busy));
    check_refs(initial.id, Phase::Shutting, 1);
    execution.vacate(ended).unwrap();
    check_refs(initial.id, Phase::Shutting, 1);
    assert_eq!(owner.begin_drain(), Err(nest::Error::Busy));
    execution.finish_end(initial.unit).unwrap();
    execution.finish_end(initial.unit).unwrap();
    assert_eq!(execution.unit_nest(initial.unit), Ok(None));
    assert_eq!(
        execution.reap(initial.unit),
        Ok(End::Returned { value: 42 })
    );
    check_refs(initial.id, Phase::Shutting, 0);

    owner.begin_drain().unwrap();
    check_refs(initial.id, Phase::Draining, 0);
    assert_eq!(execution.enter(), Err(Error::Nest(nest::Error::Shut)));
    let tree = initial.tree.borrow_mut().take().unwrap();
    let root = tree.nodes[3];
    let mut retired = None;
    assert_eq!(unsafe { owner.advance_retirement() }, Ok(Progress::Pending));
    assert_eq!(unsafe { Controls::current() }, initial.original);
    // The native predecessor is back, but the entire old tree still
    // belongs to its Draining Nest and cannot be made private.
    check_refs(initial.id, Phase::Draining, 0);
    assert_eq!(unsafe { owner.advance_retirement() }, Ok(Progress::Pending));
    assert_eq!(root.store.descriptor(root.id, 0, Fast), Err(TableError::State));
    assert_eq!(
        unsafe { root.store.retire_private(root.id, &mut retired, Fast) },
        Err(TableError::State)
    );
    assert!(retired.is_none());
    let mut previous = tree.allowance.balance().unwrap().occupied().get();
    let mut refunded = 0;
    let mut complete = false;
    for _ in 0..32 {
        let progress = unsafe { owner.advance_retirement() }.unwrap();
        let occupied = tree.allowance.balance().unwrap().occupied().get();
        assert!(occupied <= previous);
        if occupied < previous {
            assert_eq!(previous - occupied, 4096);
            let node = tree.nodes[refunded];
            assert_eq!(node.store.locate(node.allocation), Ok(Some(node.id)));
            assert_eq!(
                unsafe { node.store.retire_private(node.id, &mut retired, Fast) },
                Err(TableError::State)
            );
            assert!(retired.is_none());
            refunded += 1;
            // Even the final physical refund is not the final receipt
            // acknowledgement or the original table-position release.
            assert_eq!(progress, Progress::Pending);
        }
        previous = occupied;
        if progress == Progress::Retired {
            complete = true;
            break;
        }
        check_refs(initial.id, Phase::Draining, 0);
        assert_eq!(
            unsafe { execution.create(never) },
            Err(Error::Nest(nest::Error::Shut))
        );
    }
    assert!(complete);
    assert_eq!(refunded, 4);
    assert_eq!(NEST.observe(initial.id), Err(nest::Error::Gone));
    assert_eq!(unsafe { owner.advance_retirement() }, Ok(Progress::Retired));
    assert_eq!(owner.begin_delete(), Err(nest::Error::Gone));
    assert_eq!(
        unsafe { execution.create(never) },
        Err(Error::Nest(nest::Error::Gone))
    );
    tree.settle();
    kernel::metal::platform::boot::treaty::write(
        "PASS local-nest installed=1 rollback=1 shut=1 dormant=1 yield=1 ending=1 leave=1 refs=0 withdrawn=1 refund=4 retired=1 stale=1\n",
    );
    (initial.complete)()
}
