//! The same actual cooperative Room and Unit contract on both UEFI boards.

use core::{
    hint::{black_box, spin_loop},
    mem::{MaybeUninit, size_of},
    ptr,
    sync::atomic::{
        AtomicUsize,
        Ordering::{Acquire, Relaxed},
    },
};
use kernel::{
    basis::{
        execution::{Current, Error, Execution},
        room::Occupancy,
        unit::{End, Lifecycle, Stacks, UnitId},
    },
    entry::boot::{self, Boot, Startup},
};

#[cfg(target_arch = "aarch64")]
mod aarch64;
#[cfg(target_arch = "aarch64")]
use aarch64 as registers;
#[cfg(target_arch = "x86_64")]
mod x86_64;
#[cfg(target_arch = "x86_64")]
use x86_64 as registers;

static mut STACKS: Stacks = Stacks::new();
static mut EXECUTION: MaybeUninit<Execution> = MaybeUninit::uninit();
static MAJOR: AtomicUsize = AtomicUsize::new(usize::MAX);
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
    assert_eq!(current.hart(), super::TARGET.load(Relaxed));
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

fn minor(execution: &'static Execution) -> ! {
    super::observe_minor(super::board::hart());
    wait(1);
    // The original Hart has not returned its occupation yet.
    assert_eq!(execution.enter(), Ok(None));
    PHASE.store(2, Relaxed);
    wait(3);
    let control = registers::control();
    let receipt = dispatch(execution);
    assert_eq!(receipt.hart(), super::TARGET.load(Relaxed));
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

pub(super) fn run(
    boot: Boot<'_>,
    major: usize,
    target: usize,
    startup: Startup,
    started: impl FnOnce(),
) {
    MAJOR.store(major, Relaxed);
    let runtime = Execution::with_minor(boot, unsafe { &mut *(&raw mut STACKS) }, &super::MINOR)
        .map_err(|failure| failure.reason)
        .expect("native cooperative profile");
    let execution: &'static Execution = unsafe { (&mut *(&raw mut EXECUTION)).write(runtime) };

    assert_eq!(
        unsafe { execution.start_minor(major, minor, startup) },
        Err(Error::Busy)
    );
    unsafe { execution.start_minor(target, minor, startup) }.expect("minor admission");
    while super::OBSERVED.load(Acquire) != target {
        spin_loop();
    }
    assert!(super::MINOR.ready());
    started();
    assert_eq!(
        unsafe { execution.start_minor(target, minor, startup) },
        Err(Error::Busy)
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
    boot::write(
        "PASS cooperative-execution harts=2 migrated=2 abi=1 control=1 cancelled=1 stale=1 reused=1\n",
    );
}
