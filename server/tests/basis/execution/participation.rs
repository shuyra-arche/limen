use kernel::metal::{
    processor::treaty::{LocalStorage, Stack},
    sync::treaty::Supply,
};
pub const MAJOR_BYTES: usize = 40 * 1024;
pub const MAJOR_NODES: usize = 5;
const MINOR_BYTES: usize = 32 * 1024;
const MINOR_NODES: usize = 3;

use kernel::metal::platform::{boot::treaty::write, firmware::treaty::sbi::HartService};
use kernel::metal::processor::treaty::{Minor, StartError, Startup};

use core::{
    fmt::{self, Write},
    hint::{black_box, spin_loop},
    mem::{MaybeUninit, size_of},
    ops::Range,
    sync::atomic::{
        AtomicU8, AtomicUsize,
        Ordering::{Acquire, Relaxed, Release},
    },
};
use kernel::{
    basis::{
        execution::{Current, Error, Execution, KernelBinding, RuntimeEntry},
        room::{Occupancy, Participants},
        unit::{End, Lifecycle, Stacks, UnitId, UnitRecord},
    },
    entry::boot::Boot,
};

#[path = "../../metal/processor/execution/registers/riscv64/switch.rs"]
mod registers;
#[path = "../../metal/sync/native_snapshot.rs"]
mod snapshot;
#[path = "../../metal/sync/turn/native.rs"]
mod turn;

use kernel::metal::sync::treaty::TurnLock;
static LEAVE: TurnLock<()> = TurnLock::new(());

static mut STACKS: Stacks = Stacks::new();
static mut PARTICIPANTS: Participants<3> = Participants::new();
static mut DISPATCHERS: kernel::metal::processor::treaty::Dispatchers<Occupancy, 3> =
    kernel::metal::processor::treaty::Dispatchers::new();
static mut RECORDS: [UnitRecord<KernelBinding>; 4] = [const { UnitRecord::EMPTY }; 4];
static MINOR_STACK: Stack<MINOR_BYTES> = Stack::new();
static MINOR_LOCAL: LocalStorage<MINOR_NODES> = LocalStorage::new(Supply::new());
static MINOR: Minor = Minor::new(&MINOR_STACK, &MINOR_LOCAL);
static THIRD_STACK: Stack<{ 32 * 1024 }> = Stack::new();
static THIRD_LOCAL: LocalStorage<5> = LocalStorage::new(Supply::new());
static THIRD: Minor = Minor::new(&THIRD_STACK, &THIRD_LOCAL);
static FOURTH_STACK: Stack<{ 32 * 1024 }> = Stack::new();
static FOURTH_LOCAL: LocalStorage<5> = LocalStorage::new(Supply::new());
static FOURTH: Minor = Minor::new(&FOURTH_STACK, &FOURTH_LOCAL);
static EXTRA: AtomicU8 = AtomicU8::new(0);
static mut EXECUTION: MaybeUninit<Execution> = MaybeUninit::uninit();
static mut MAJOR_RUNTIME: MaybeUninit<RuntimeEntry<Initial>> = MaybeUninit::uninit();
static MINOR_RUNTIME: RuntimeEntry<()> = RuntimeEntry::new(schedule_minor, ());
static mut THIRD_RUNTIME: MaybeUninit<RuntimeEntry<usize>> = MaybeUninit::uninit();
static FOURTH_RUNTIME: RuntimeEntry<()> = RuntimeEntry::new(unreachable_runtime, ());
static mut SCENARIO: MaybeUninit<Scenario> = MaybeUninit::uninit();
static mut PREVIOUS: MaybeUninit<Occupancy> = MaybeUninit::uninit();
static FIRST_STACK: AtomicUsize = AtomicUsize::new(0);

static OBSERVED: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Copy)]
struct Scenario {
    first: UnitId,
    major: usize,
    minor: usize,
}

#[repr(u8)]
enum Phase {
    AffinityChecked = 1,
    FirstRunning,
    BothRunning,
    FirstStopped,
    WithheldChecked,
    OldVacated,
    MigratedStopped,
    ReplayChecked,
    MigratedEnded,
}

static PHASE: AtomicU8 = AtomicU8::new(0);

fn publish(phase: Phase) {
    PHASE.store(phase as u8, Release);
}

fn wait(phase: Phase) {
    let phase = phase as u8;
    while PHASE.load(Acquire) < phase {
        spin_loop();
    }
}

fn scenario() -> Scenario {
    // Published by the first Unit's FirstRunning phase before the peer reads.
    unsafe { (&*(&raw const SCENARIO)).assume_init_read() }
}

fn stack_address(value: &usize) -> usize {
    let address = core::ptr::from_ref(black_box(value)).addr();
    let base = (&raw const STACKS).addr();
    assert!((base..base + size_of::<Stacks>()).contains(&address));
    address
}

fn first(current: &mut Current) -> u64 {
    let Scenario {
        first,
        major,
        minor,
    } = scenario();
    let local = black_box(0x1234_5678usize);
    assert_eq!(current.id(), first);
    assert_eq!(current.processor(), major);
    FIRST_STACK.store(stack_address(&local), Relaxed);
    observe(major);
    publish(Phase::FirstRunning);

    wait(Phase::BothRunning);
    let rejected = LEAVE
        .acquire(|_| current.yield_now())
        .expect("occupied Unit can acquire an ordinary Turn");
    assert_eq!(rejected, Err(Error::Protection));
    current
        .yield_now()
        .expect("leave with an independent Unit on the other processor");
    assert_eq!(current.id(), first);
    assert_eq!(current.processor(), minor);
    assert_eq!(stack_address(&local), FIRST_STACK.load(Relaxed));
    assert_eq!(black_box(local), 0x1234_5678);
    local as u64
}

fn second(current: &mut Current) -> u64 {
    let minor = scenario().minor;
    let local = black_box(41usize);
    assert_eq!(current.processor(), minor);
    assert!(stack_address(&local).abs_diff(FIRST_STACK.load(Relaxed)) >= 8 * 1024);
    observe(minor);
    publish(Phase::BothRunning);

    wait(Phase::FirstStopped);
    local as u64
}

fn pinned(current: &mut Current) -> u64 {
    assert_eq!(current.processor(), scenario().major);
    17
}

fn dispatch(execution: &'static Execution, unit: UnitId, processor: usize) -> Occupancy {
    let occupancy = execution.enter().expect("dispatch").expect("ready unit");
    assert_eq!(occupancy.unit(), unit);
    assert_eq!(occupancy.processor(), processor);
    assert_eq!(execution.pending_leave(), Ok(Some(occupancy)));
    occupancy
}

fn finish(execution: &Execution, receipt: Occupancy, value: u64) {
    let unit = receipt.unit();
    assert_eq!(execution.lifecycle(unit), Ok(Lifecycle::Ending));
    assert_eq!(execution.finish_end(unit), Err(Error::Busy));
    assert_eq!(execution.reap(unit), Err(Error::UnitNotEnded));
    execution.vacate(receipt).expect("matching leave");
    execution.finish_end(unit).expect("released admission");
    assert_eq!(
        execution.lifecycle(unit),
        Ok(Lifecycle::Ended(End::Returned { value }))
    );
    assert_eq!(execution.reap(unit), Ok(End::Returned { value }));
}

fn launched(argument: usize) -> &'static Execution {
    // Boot's launch release covers the resident Execution initialization.
    unsafe { &*core::ptr::with_exposed_provenance::<Execution>(argument) }
}

fn execution() -> &'static Execution {
    // The launch records are published after this original initialization.
    unsafe { (&*(&raw const EXECUTION)).assume_init_ref() }
}

fn run_minor(_: usize, argument: usize) -> ! {
    let execution = launched(argument);
    assert_eq!(execution.enter(), Err(Error::Unsupported));
    unsafe { execution.run(&MINOR_RUNTIME) }.expect("resident dispatcher")
}

fn schedule_minor(_: &'static ()) -> ! {
    let execution = execution();
    let mark = black_box(0u8);
    let address = core::ptr::from_ref(black_box(&mark)).addr();
    assert!(!MINOR.stack_range().contains(&address));
    assert_eq!(execution.enter(), Err(Error::Unsupported));
    execution.attach().expect("attach actual minor processor");
    assert_eq!(execution.attach(), Err(Error::Busy));

    turn::minor::<MINOR_NODES, { MINOR_NODES + 1 }, { MINOR_NODES - 1 }>();
    snapshot::observe();
    assert_eq!(
        execution.enter(),
        Ok(None),
        "minor must reject the Ready Unit pinned to major"
    );
    publish(Phase::AffinityChecked);
    wait(Phase::FirstRunning);
    let Scenario { first, minor, .. } = scenario();
    assert_eq!(execution.enter(), Ok(None));
    assert_eq!(execution.lifecycle(first), Ok(Lifecycle::Live));
    assert_eq!(execution.finish_end(first), Err(Error::Busy));
    assert_eq!(execution.submit(first), Err(Error::Busy));

    let other = unsafe { execution.create(second) }.expect("create on minor");
    execution.submit(other).expect("admit on minor");
    finish(execution, dispatch(execution, other, minor), 41);
    // The original Unit left its stack. Its exact receipt remains unconsumed.
    assert_eq!(execution.enter(), Ok(None));
    assert_eq!(execution.pending_leave(), Ok(None));
    assert_eq!(execution.finish_end(first), Err(Error::Busy));
    publish(Phase::WithheldChecked);

    wait(Phase::OldVacated);
    let migrated = dispatch(execution, first, minor);
    // The major wrote PREVIOUS once and published it in FirstStopped.
    let previous = unsafe { (&*(&raw const PREVIOUS)).assume_init_read() };
    assert_ne!(migrated, previous);
    assert_eq!(migrated.admission(), previous.admission());
    assert_eq!(execution.vacate(previous), Err(Error::Gone));
    assert_eq!(execution.pending_leave(), Ok(Some(migrated)));
    publish(Phase::MigratedStopped);

    wait(Phase::ReplayChecked);
    assert_eq!(execution.pending_leave(), Ok(Some(migrated)));
    finish(execution, migrated, 0x1234_5678);
    publish(Phase::MigratedEnded);
    loop {
        spin_loop();
    }
}

fn run_third(processor: usize, argument: usize) -> ! {
    let execution = launched(argument);
    assert_eq!(execution.enter(), Err(Error::Unsupported));
    let entry = unsafe {
        (&mut *(&raw mut THIRD_RUNTIME)).write(RuntimeEntry::new(schedule_third, processor))
    };
    unsafe { execution.run(entry) }.expect("resident dispatcher")
}

fn schedule_third(&processor: &'static usize) -> ! {
    let execution = execution();
    let mark = black_box(0u8);
    let address = core::ptr::from_ref(black_box(&mark)).addr();
    assert!(!THIRD.stack_range().contains(&address));
    assert_eq!(execution.enter(), Err(Error::Unsupported));
    execution
        .attach()
        .expect("third supplied participation slot");
    observe(processor);
    assert_eq!(execution.attach(), Err(Error::Busy));

    let unit = unsafe { execution.create(|current| current.processor() as u64) }
        .expect("create on third processor");
    execution.submit(unit).expect("admit on third processor");
    finish(
        execution,
        dispatch(execution, unit, processor),
        processor as u64,
    );
    EXTRA.store(1, Release);
    loop {
        spin_loop();
    }
}

fn unreachable_runtime(_: &'static ()) -> ! {
    panic!("fourth processor exceeded resident dispatcher capacity")
}

fn run_fourth(_: usize, argument: usize) -> ! {
    let execution = launched(argument);
    assert_eq!(
        unsafe { execution.run(&FOURTH_RUNTIME) },
        Err(Error::Capacity)
    );
    assert_eq!(execution.attach(), Err(Error::Unsupported));
    assert_eq!(execution.enter(), Err(Error::Unsupported));
    assert_eq!(execution.pending_leave(), Err(Error::Unsupported));
    EXTRA.store(2, Release);
    loop {
        spin_loop();
    }
}

pub fn check(mut boot: Boot<'_>, complete: fn() -> !) -> ! {
    assert_eq!(boot.stack_range().len(), MAJOR_BYTES);
    assert_eq!(MINOR.stack_range(), MINOR_STACK.range());
    assert!(
        boot.stack_range().end <= MINOR.stack_range().start
            || MINOR.stack_range().end <= boot.stack_range().start
    );
    let major = boot.processor();
    assert!(major < 4, "unexpected QEMU Boot processor");
    for processor in 0..4 {
        assert!(boot.riscv_cpus().cpu(processor).unwrap().unwrap().enabled());
    }
    assert!(boot.riscv_cpus().cpu(4).unwrap().is_none());
    let mut others = (0..4).filter(|&processor| processor != major);
    let minor_processor = others.next().unwrap();
    let third = others.next().unwrap();
    let fourth = others.next().unwrap();
    let service = boot
        .sbi_services()
        .hart_state_management()
        .unwrap()
        .expect("SBI HSM");
    let major_stack = boot.stack_range();
    let runtime = Execution::new(
        boot.take_major().expect("original native major relation"),
        boot.terminal(),
        unsafe { &mut *(&raw mut STACKS) },
        unsafe { &mut *(&raw mut RECORDS) },
        unsafe { &mut *(&raw mut PARTICIPANTS) },
        unsafe { &mut *(&raw mut DISPATCHERS) },
    )
    .map_err(|failure| failure.reason)
    .expect("Bare scalar execution");
    // The platform owns the resident objects and initializes them once.
    let execution: &'static Execution = unsafe { (&mut *(&raw mut EXECUTION)).write(runtime) };
    assert_eq!(execution.attach(), Err(Error::Unsupported));
    let initial = Initial {
        major_stack,
        major,
        minor_processor,
        third,
        fourth,
        service,
        complete,
    };
    let entry = unsafe {
        (&mut *(&raw mut MAJOR_RUNTIME)).write(RuntimeEntry::new(schedule_major, initial))
    };
    unsafe { execution.run(entry) }.expect("resident dispatcher")
}

struct Initial {
    major_stack: Range<usize>,
    major: usize,
    minor_processor: usize,
    third: usize,
    fourth: usize,
    service: HartService,
    complete: fn() -> !,
}

fn schedule_major(initial: &'static Initial) -> ! {
    let execution = execution();
    let mark = black_box(0u8);
    let address = core::ptr::from_ref(black_box(&mark)).addr();
    assert!(!initial.major_stack.contains(&address));
    assert_eq!(execution.enter(), Err(Error::Unsupported));
    execution.attach().expect("attach actual boot processor");
    assert_eq!(execution.attach(), Err(Error::Busy));
    let Initial {
        major,
        minor_processor,
        third,
        fourth,
        service,
        complete,
        ..
    } = *initial;

    let argument = core::ptr::from_ref(execution).expose_provenance();
    // The continuation holds scalar locals, the Unit stack and shared fixture
    // state across Yield. It holds no processor-local resource.
    let a = unsafe { execution.create_migratable(first) }.expect("migratable Unit");
    let pinned = unsafe { execution.create(pinned) }.expect("create on major");
    execution.submit(pinned).expect("admit pinned Unit");
    assert!(matches!(
        unsafe { MINOR.start(8, run_minor, argument, &mut Startup::Sbi(service)) },
        Err(StartError::Rejected(_))
    ));
    unsafe {
        (&mut *(&raw mut SCENARIO)).write(Scenario {
            first: a,
            major,
            minor: minor_processor,
        });
        MINOR
            .start(
                minor_processor,
                run_minor,
                argument,
                &mut Startup::Sbi(service),
            )
            .expect("start actual minor processor");
    }
    turn::major::<MAJOR_NODES, { MAJOR_NODES + 1 }>();
    snapshot::publish();
    wait(Phase::AffinityChecked);
    finish(execution, dispatch(execution, pinned, major), 17);
    execution.submit(a).expect("admit migratable Unit");

    let previous = dispatch(execution, a, major);
    unsafe { (&mut *(&raw mut PREVIOUS)).write(previous) };
    publish(Phase::FirstStopped);
    wait(Phase::WithheldChecked);
    assert_eq!(execution.pending_leave(), Ok(Some(previous)));
    execution
        .vacate(previous)
        .expect("major settles original Yield");
    publish(Phase::OldVacated);

    wait(Phase::MigratedStopped);
    assert_eq!(execution.vacate(previous), Err(Error::Gone));
    assert_eq!(execution.finish_end(a), Err(Error::Busy));
    assert_eq!(execution.enter(), Ok(None));
    publish(Phase::ReplayChecked);
    wait(Phase::MigratedEnded);
    assert_eq!(execution.lifecycle(a), Err(Error::Gone));
    assert_eq!(registers::migrations(), 1);

    // Three participants have independent machine records while the Unit
    // stack supply still has only two positions. The fourth actual entry
    // cannot publish a partial participation or a local dispatcher locator.
    unsafe { THIRD.start(third, run_third, argument, &mut Startup::Sbi(service)) }
        .expect("start third actual Hart");
    while EXTRA.load(Acquire) != 1 {
        spin_loop();
    }
    assert!(THIRD.ready());
    unsafe { FOURTH.start(fourth, run_fourth, argument, &mut Startup::Sbi(service)) }
        .expect("start fourth actual Hart");
    while EXTRA.load(Acquire) != 2 {
        spin_loop();
    }
    assert!(FOURTH.ready());

    assert_eq!(
        OBSERVED.load(Acquire),
        (1 << major) | (1 << minor_processor) | (1 << third)
    );
    write(
        "PASS kernel-execution-participants active=2 turn=1 snap=1 affinity=1 pending=1 migrated=1 registers=12 stale=1 ended=3 rollback=1 participants=3 capacity=1\n",
    );
    complete()
}

struct Console;

impl Write for Console {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        write(text);
        Ok(())
    }
}

fn observe(processor: usize) {
    assert!(processor < 4);
    let bit = 1 << processor;
    assert_eq!(OBSERVED.fetch_or(bit, Release) & bit, 0);
    let _ = writeln!(
        Console,
        "OBSERVED kernel-execution-participants processor={processor}"
    );
}
