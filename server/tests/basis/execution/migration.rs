//! Cross-Hart Unit migration, independent of firmware admission.

use core::{
    hint::{black_box, spin_loop},
    mem::{MaybeUninit, size_of},
    ptr,
    sync::atomic::{
        AtomicUsize,
        Ordering::{Acquire, Relaxed, Release},
    },
};
use kernel::metal::platform::boot::treaty as boot_diagnostic;
use kernel::metal::{
    processor::treaty::{LocalStorage, Minor, Stack, StartError, Startup},
    sync::treaty::Supply,
};
use kernel::{
    basis::{
        execution::{Current, Error, Execution, KernelBinding},
        room::{Harts, Occupancy},
        unit::{End, Lifecycle, Stacks, UnitId, UnitRecord},
    },
    entry::boot::Boot,
};

#[cfg(target_arch = "aarch64")]
#[path = "../../metal/processor/execution/registers/aarch64.rs"]
mod aarch64;
#[cfg(target_arch = "aarch64")]
use aarch64 as registers;
#[cfg(target_arch = "x86_64")]
#[path = "../../metal/processor/execution/registers/x86_64.rs"]
mod x86_64;
#[cfg(target_arch = "x86_64")]
use x86_64 as registers;

static mut STACKS: Stacks = Stacks::new();
static mut HARTS: Harts<2> = Harts::new();
static mut DISPATCHERS: kernel::metal::processor::treaty::Dispatchers<Occupancy, 2> =
    kernel::metal::processor::treaty::Dispatchers::new();
static mut RECORDS: [UnitRecord<KernelBinding>; 4] = [const { UnitRecord::EMPTY }; 4];
static mut EXECUTION: MaybeUninit<Execution> = MaybeUninit::uninit();
static MAJOR: AtomicUsize = AtomicUsize::new(usize::MAX);
static PEER: AtomicUsize = AtomicUsize::new(usize::MAX);
static MINOR_STACK: Stack<{ 32 * 1024 }> = Stack::new();
static MINOR_LOCAL: LocalStorage<5> = LocalStorage::new(Supply::new());
pub static MINOR: Minor = Minor::new(&MINOR_STACK, &MINOR_LOCAL);
static OBSERVED: AtomicUsize = AtomicUsize::new(usize::MAX);

#[path = "../../metal/sync/native.rs"]
mod synchronization;

struct Launch {
    execution: &'static Execution,
    observe: fn(&Minor, usize),
}
static PHASE: AtomicUsize = AtomicUsize::new(0);
static STEP: AtomicUsize = AtomicUsize::new(0);
static STACK: AtomicUsize = AtomicUsize::new(0);

fn wait(phase: usize) {
    while PHASE.load(Relaxed) != phase {
        spin_loop();
    }
}

fn stack_address(value: &usize) -> usize {
    let address = ptr::from_ref(black_box(value)).addr();
    let base = (&raw const STACKS).addr();
    assert!((base..base + size_of::<Stacks>()).contains(&address));
    address
}

fn migratable(current: &mut Current) -> u64 {
    let value = black_box(0x1234_5678usize);
    let address = stack_address(&value);
    STACK.store(address, Relaxed);
    assert_eq!(current.hart(), MAJOR.load(Relaxed));
    assert_eq!(STEP.fetch_add(1, Relaxed), 0);
    let original = registers::control();
    let changed = registers::rounding(original);
    registers::install(changed);
    assert_eq!(registers::control(), changed);

    registers::verify(current);
    assert_eq!(current.hart(), PEER.load(Relaxed));
    assert_eq!(STEP.fetch_add(1, Relaxed), 1);
    assert_eq!(stack_address(&value), address);
    assert_eq!(registers::control(), changed);

    registers::verify(current);
    assert_eq!(current.hart(), MAJOR.load(Relaxed));
    assert_eq!(STEP.fetch_add(1, Relaxed), 2);
    assert_eq!(stack_address(&value), address);
    assert_eq!(registers::control(), changed);
    registers::install(original);
    black_box(value) as u64
}

fn never(_: &mut Current) -> u64 {
    panic!("cancelled Unit entered");
}

fn replacement(_: &mut Current) -> u64 {
    let value = black_box(41usize);
    assert!(stack_address(&value).abs_diff(STACK.load(Relaxed)) < 16 * 1024);
    value as u64
}

fn dispatch(execution: &'static Execution) -> Occupancy {
    loop {
        if let Some(occupancy) = execution.enter().expect("actual dispatch") {
            assert_eq!(execution.pending_leave(), Ok(Some(occupancy)));
            return occupancy;
        }
        spin_loop();
    }
}

fn minor(hart: usize, argument: usize) -> ! {
    // Minor::start publishes the borrowed, immutable launch record. The
    // nonreturning major retains it for this entire machine execution.
    let launch = unsafe { &*ptr::with_exposed_provenance::<Launch>(argument) };
    let execution = launch.execution;
    assert_eq!(execution.enter(), Err(Error::Unsupported));
    execution.attach().expect("attach actual minor Hart");
    assert_eq!(execution.attach(), Err(Error::Busy));
    assert_eq!(hart, PEER.load(Relaxed));
    (launch.observe)(&MINOR, hart);
    synchronization::check();
    OBSERVED.store(hart, Release);
    assert_eq!(execution.enter(), Err(Error::Protection));
    let mut schedule = || {
        let mark = core::hint::black_box(0u8);
        let address = core::hint::black_box(&mark) as *const u8 as usize;
        assert!(!MINOR.stack_range().contains(&address));
        wait(1);
        // The original Hart has not returned its occupation yet.
        assert_eq!(execution.enter(), Ok(None));
        PHASE.store(2, Relaxed);
        wait(3);
        let control = registers::control();
        let receipt = dispatch(execution);
        assert_eq!(receipt.hart(), PEER.load(Relaxed));
        assert_eq!(registers::control(), control);
        execution.vacate(receipt).expect("minor departure");
        PHASE.store(4, Relaxed);
        loop {
            spin_loop();
        }
    };
    unsafe { execution.run(&mut schedule) }.expect("resident dispatcher")
}

fn finish(execution: &Execution, unit: UnitId, receipt: Occupancy, value: u64) {
    assert_eq!(receipt.unit(), unit);
    assert_eq!(execution.lifecycle(unit), Ok(Lifecycle::Ending));
    assert_eq!(execution.finish_end(unit), Err(Error::Busy));
    execution
        .vacate(receipt)
        .expect("matching terminal departure");
    execution.finish_end(unit).expect("closed admission");
    assert_eq!(execution.reap(unit), Ok(End::Returned { value }));
    assert_eq!(execution.lifecycle(unit), Err(Error::Gone));
}

pub fn run(
    mut boot: Boot<'_>,
    target: usize,
    startup: &mut Startup,
    observe: fn(&Minor, usize),
    mut started: impl FnMut(&Startup),
    complete: fn() -> !,
) -> ! {
    let major = boot.hart();
    assert_ne!(target, major);
    MAJOR.store(major, Relaxed);
    PEER.store(target, Relaxed);
    let major_stack = boot.stack_range();
    let runtime = Execution::new(
        boot.take_major().expect("original native major relation"),
        boot.terminal(),
        unsafe { &mut *(&raw mut STACKS) },
        unsafe { &mut *(&raw mut RECORDS) },
        unsafe { &mut *(&raw mut HARTS) },
        unsafe { &mut *(&raw mut DISPATCHERS) },
    )
    .map_err(|failure| failure.reason)
    .expect("native cooperative profile");
    let execution: &'static Execution = unsafe { (&mut *(&raw mut EXECUTION)).write(runtime) };
    let launch = Launch { execution, observe };
    execution.attach().expect("attach actual boot Hart");
    assert_eq!(execution.attach(), Err(Error::Busy));
    assert_eq!(execution.enter(), Err(Error::Protection));
    let mut schedule = || {
        let mark = core::hint::black_box(0u8);
        let address = core::hint::black_box(&mark) as *const u8 as usize;
        assert!(!major_stack.contains(&address));
        let argument = ptr::from_ref(&launch).expose_provenance();

        assert_eq!(
            unsafe { MINOR.start(major, minor, argument, startup) },
            Err(StartError::Busy)
        );
        unsafe { MINOR.start(target, minor, argument, startup) }.expect("minor startup");
        while OBSERVED.load(Acquire) != target {
            spin_loop();
        }
        assert!(MINOR.ready());
        started(startup);
        assert_eq!(
            unsafe { MINOR.start(target, minor, argument, startup) },
            Err(StartError::Busy)
        );

        let unit = unsafe { execution.create_migratable(migratable) }.expect("migratable Unit");
        let dormant = unsafe { execution.create(never) }.expect("dormant Unit");
        execution
            .stop_dormant(dormant)
            .expect("cancel before admission");
        assert_eq!(execution.submit(dormant), Err(Error::UnitNotLive));
        execution.finish_end(dormant).unwrap();
        assert_eq!(execution.reap(dormant), Ok(End::Cancelled));
        execution.submit(unit).unwrap();

        let control = registers::control();
        let first = dispatch(execution);
        assert_eq!(first.unit(), unit);
        assert_eq!(first.hart(), major);
        assert_eq!(registers::control(), control);
        PHASE.store(1, Relaxed);
        wait(2);
        execution.vacate(first).expect("major departure");
        PHASE.store(3, Relaxed);
        wait(4);
        let last = dispatch(execution);
        assert_eq!(last.hart(), major);
        assert_eq!(registers::control(), control);
        assert_eq!(execution.vacate(first), Err(Error::Gone));
        finish(execution, unit, last, 0x1234_5678);
        assert_eq!(STEP.load(Relaxed), 3);

        let reused = unsafe { execution.create(replacement) }.expect("returned stack");
        assert_ne!(reused, unit);
        execution.submit(reused).unwrap();
        let receipt = dispatch(execution);
        finish(execution, reused, receipt, 41);
        assert_eq!(execution.enter(), Ok(None));
        boot_diagnostic::write(
            "PASS cooperative-execution harts=2 migrated=2 abi=1 control=1 cancelled=1 stale=1 reused=1\n",
        );
        complete()
    };
    unsafe { execution.run(&mut schedule) }.expect("resident dispatcher")
}
