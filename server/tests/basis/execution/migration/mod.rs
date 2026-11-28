//! Cross-processor Unit migration and its firmware-specific admission.

#[cfg(target_arch = "riscv64")]
mod sbi;
#[cfg(target_os = "uefi")]
mod uefi;
#[cfg(target_arch = "riscv64")]
pub use sbi::check;
#[cfg(target_os = "uefi")]
pub use uefi::{check, prepare};

use core::{
    hint::{black_box, spin_loop},
    mem::{MaybeUninit, size_of},
    ops::Range,
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
        execution::{Current, Error, Execution, KernelBinding, RuntimeEntry},
        room::{Occupancy, Participants},
        unit::{End, Lifecycle, Stacks, UnitId, UnitRecord},
    },
    entry::boot::Boot,
};

#[cfg(target_arch = "aarch64")]
#[path = "../../../metal/processor/execution/registers/aarch64.rs"]
mod aarch64;
#[cfg(target_arch = "aarch64")]
use aarch64 as registers;
#[cfg(target_arch = "x86_64")]
#[path = "../../../metal/processor/execution/registers/x86_64.rs"]
mod x86_64;
#[cfg(target_arch = "x86_64")]
use x86_64 as registers;
#[cfg(target_arch = "riscv64")]
#[path = "../../../metal/processor/execution/registers/riscv64/mod.rs"]
mod riscv64;
#[cfg(target_arch = "riscv64")]
use riscv64 as registers;

static mut STACKS: Stacks = Stacks::new();
static mut PARTICIPANTS: Participants<2> = Participants::new();
static mut DISPATCHERS: kernel::metal::processor::treaty::Dispatchers<Occupancy, 2> =
    kernel::metal::processor::treaty::Dispatchers::new();
static mut RECORDS: [UnitRecord<KernelBinding>; 4] = [const { UnitRecord::EMPTY }; 4];
static mut EXECUTION: MaybeUninit<Execution> = MaybeUninit::uninit();
static mut MAJOR_RUNTIME: MaybeUninit<RuntimeEntry<Initial>> = MaybeUninit::uninit();
static mut LAUNCH: MaybeUninit<Launch> = MaybeUninit::uninit();
static MINOR_RUNTIME: RuntimeEntry<()> = RuntimeEntry::new(schedule_minor, ());
static MAJOR: AtomicUsize = AtomicUsize::new(usize::MAX);
static PEER: AtomicUsize = AtomicUsize::new(usize::MAX);
static MINOR_STACK: Stack<{ 32 * 1024 }> = Stack::new();
static MINOR_LOCAL: LocalStorage<5> = LocalStorage::new(Supply::new());
static MINOR: Minor = Minor::new(&MINOR_STACK, &MINOR_LOCAL);
static OBSERVED: AtomicUsize = AtomicUsize::new(usize::MAX);

#[path = "../../../metal/sync/native.rs"]
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
    assert_eq!(current.processor(), MAJOR.load(Relaxed));
    assert_eq!(STEP.fetch_add(1, Relaxed), 0);
    let original = registers::control();
    #[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
    let expected = {
        let changed = registers::rounding(original);
        registers::install(changed);
        assert_eq!(registers::control(), changed);
        changed
    };
    #[cfg(target_arch = "riscv64")]
    let expected = original;

    registers::verify(current);
    assert_eq!(current.processor(), PEER.load(Relaxed));
    assert_eq!(STEP.fetch_add(1, Relaxed), 1);
    assert_eq!(stack_address(&value), address);
    assert_eq!(registers::control(), expected);

    registers::verify(current);
    assert_eq!(current.processor(), MAJOR.load(Relaxed));
    assert_eq!(STEP.fetch_add(1, Relaxed), 2);
    assert_eq!(stack_address(&value), address);
    assert_eq!(registers::control(), expected);
    #[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
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

fn execution() -> &'static Execution {
    // The trusted platform initializes this before releasing the minor.
    unsafe { (&*(&raw const EXECUTION)).assume_init_ref() }
}

fn minor(processor: usize, argument: usize) -> ! {
    // Minor::start publishes the resident, immutable launch record.
    let launch = unsafe { &*ptr::with_exposed_provenance::<Launch>(argument) };
    let execution = launch.execution;
    assert_eq!(execution.enter(), Err(Error::Unsupported));
    assert_eq!(execution.attach(), Err(Error::Unsupported));
    assert_eq!(processor, PEER.load(Relaxed));
    (launch.observe)(&MINOR, processor);
    synchronization::check();
    #[cfg(target_arch = "riscv64")]
    synchronization::check_enabled();
    unsafe { execution.run(&MINOR_RUNTIME) }.expect("resident dispatcher")
}

fn schedule_minor(_: &'static ()) -> ! {
    let execution = execution();
    let mark = black_box(0u8);
    let address = ptr::from_ref(black_box(&mark)).addr();
    assert!(!MINOR.stack_range().contains(&address));
    assert_eq!(execution.enter(), Err(Error::Unsupported));
    execution.attach().expect("attach actual minor processor");
    assert_eq!(execution.attach(), Err(Error::Busy));
    OBSERVED.store(PEER.load(Relaxed), Release);

    wait(1);
    // The original processor has not returned its occupation yet.
    assert_eq!(execution.enter(), Ok(None));
    PHASE.store(2, Relaxed);
    wait(3);
    let control = registers::control();
    let receipt = dispatch(execution);
    assert_eq!(receipt.processor(), PEER.load(Relaxed));
    assert_eq!(registers::control(), control);
    execution.vacate(receipt).expect("minor departure");
    PHASE.store(4, Relaxed);
    loop {
        spin_loop();
    }
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

fn run(
    mut boot: Boot<'_>,
    target: usize,
    mut startup: Startup,
    observe: fn(&Minor, usize),
    started: impl FnOnce(&Startup),
    complete: fn() -> !,
) -> ! {
    let major = boot.processor();
    assert_ne!(target, major);
    MAJOR.store(major, Relaxed);
    PEER.store(target, Relaxed);
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
    .expect("native cooperative profile");
    let execution: &'static Execution = unsafe { (&mut *(&raw mut EXECUTION)).write(runtime) };
    let launch = unsafe { (&mut *(&raw mut LAUNCH)).write(Launch { execution, observe }) };
    let argument = ptr::from_ref(launch).expose_provenance();
    assert_eq!(
        unsafe { MINOR.start(major, minor, argument, &mut startup) },
        Err(StartError::Busy)
    );
    unsafe { MINOR.start(target, minor, argument, &mut startup) }.expect("minor startup");
    while OBSERVED.load(Acquire) != target {
        spin_loop();
    }
    assert!(MINOR.ready());
    started(&startup);
    assert_eq!(
        unsafe { MINOR.start(target, minor, argument, &mut startup) },
        Err(StartError::Busy)
    );

    let initial = Initial {
        major,
        major_stack,
        complete,
    };
    let entry = unsafe {
        (&mut *(&raw mut MAJOR_RUNTIME)).write(RuntimeEntry::new(schedule_major, initial))
    };
    unsafe { execution.run(entry) }.expect("resident dispatcher")
}

struct Initial {
    major: usize,
    major_stack: Range<usize>,
    complete: fn() -> !,
}

fn schedule_major(initial: &'static Initial) -> ! {
    let execution = execution();
    let mark = black_box(0u8);
    let address = ptr::from_ref(black_box(&mark)).addr();
    assert!(!initial.major_stack.contains(&address));
    assert_eq!(execution.enter(), Err(Error::Unsupported));
    execution.attach().expect("attach actual boot processor");
    assert_eq!(execution.attach(), Err(Error::Busy));
    let major = initial.major;

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
    assert_eq!(first.processor(), major);
    assert_eq!(registers::control(), control);
    PHASE.store(1, Relaxed);
    wait(2);
    execution.vacate(first).expect("major departure");
    PHASE.store(3, Relaxed);
    wait(4);
    let last = dispatch(execution);
    assert_eq!(last.unit(), unit);
    assert_ne!(last, first);
    assert_eq!(last.admission(), first.admission());
    assert_eq!(last.processor(), major);
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
        "PASS cooperative-execution processors=2 migrated=2 abi=1 control=1 cancelled=1 stale=1 reused=1\n",
    );
    (initial.complete)()
}
