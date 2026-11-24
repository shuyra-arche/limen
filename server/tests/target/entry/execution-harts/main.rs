#![no_std]
#![no_main]

use core::{
    arch::global_asm,
    fmt::{self, Write},
    hint::{black_box, spin_loop},
    mem::{MaybeUninit, size_of},
    panic::PanicInfo,
    sync::atomic::{
        AtomicU8, AtomicUsize,
        Ordering::{Acquire, Relaxed, Release},
    },
};
use kernel::entry::{
    boot::{Boot, Minor, shutdown, write},
    execution::{Current, End, Error, Execution, Lifecycle, Occupancy, Stacks, UnitId},
};

global_asm!(include_str!("register-probe.S"));

static mut STACKS: Stacks = Stacks::new();
static MINOR: Minor = Minor::new();
static mut EXECUTION: MaybeUninit<Execution> = MaybeUninit::uninit();
static mut SCENARIO: MaybeUninit<Scenario> = MaybeUninit::uninit();
static mut PREVIOUS: MaybeUninit<Occupancy> = MaybeUninit::uninit();
static FIRST_STACK: AtomicUsize = AtomicUsize::new(0);

#[unsafe(no_mangle)]
static limen_register_probe_migrated: AtomicUsize = AtomicUsize::new(0);

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
    assert_eq!(current.hart(), major);
    FIRST_STACK.store(stack_address(&local), Relaxed);
    observe(major);
    publish(Phase::FirstRunning);

    wait(Phase::BothRunning);
    current
        .yield_now()
        .expect("leave with an independent Unit on the other Hart");
    assert_eq!(current.id(), first);
    assert_eq!(current.hart(), minor);
    assert_eq!(stack_address(&local), FIRST_STACK.load(Relaxed));
    assert_eq!(black_box(local), 0x1234_5678);
    local as u64
}

fn second(current: &mut Current) -> u64 {
    let minor = scenario().minor;
    let local = black_box(41usize);
    assert_eq!(current.hart(), minor);
    assert!(stack_address(&local).abs_diff(FIRST_STACK.load(Relaxed)) >= 8 * 1024);
    observe(minor);
    publish(Phase::BothRunning);

    wait(Phase::FirstStopped);
    local as u64
}

fn pinned(current: &mut Current) -> u64 {
    assert_eq!(current.hart(), scenario().major);
    17
}

fn dispatch(execution: &'static Execution, unit: UnitId, hart: usize) -> Occupancy {
    let occupancy = execution.enter().expect("dispatch").expect("ready unit");
    assert_eq!(occupancy.unit(), unit);
    assert_eq!(occupancy.hart(), hart);
    assert_eq!(execution.pending_leave(), Some(occupancy));
    occupancy
}

fn finish(execution: &Execution, receipt: Occupancy, value: u64) {
    let unit = receipt.unit();
    assert_eq!(execution.lifecycle(unit), Ok(Lifecycle::Ending));
    assert_eq!(execution.finish_end(unit), Err(Error::Busy));
    assert_eq!(execution.reap(unit), Err(Error::State));
    execution.vacate(receipt).expect("matching leave");
    execution.finish_end(unit).expect("released admission");
    assert_eq!(
        execution.lifecycle(unit),
        Ok(Lifecycle::Ended(End::Returned { value }))
    );
    assert_eq!(execution.reap(unit), Ok(End::Returned { value }));
}

fn run_minor(execution: &'static Execution) -> ! {
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
    assert_eq!(execution.pending_leave(), None);
    assert_eq!(execution.finish_end(first), Err(Error::Busy));
    publish(Phase::WithheldChecked);

    wait(Phase::OldVacated);
    let migrated = dispatch(execution, first, minor);
    // The major wrote PREVIOUS once and published it in FirstStopped.
    let previous = unsafe { (&*(&raw const PREVIOUS)).assume_init_read() };
    assert_ne!(migrated, previous);
    assert_eq!(execution.vacate(previous), Err(Error::Gone));
    assert_eq!(execution.pending_leave(), Some(migrated));
    publish(Phase::MigratedStopped);

    wait(Phase::ReplayChecked);
    assert_eq!(execution.pending_leave(), Some(migrated));
    finish(execution, migrated, 0x1234_5678);
    publish(Phase::MigratedEnded);
    loop {
        spin_loop();
    }
}

#[unsafe(no_mangle)]
extern "Rust" fn limen_platform_ready(boot: Boot) -> ! {
    assert_eq!(
        MINOR.stack_range().len(),
        kernel::entry::boot::stack_range().len()
    );
    assert_eq!(MINOR.stack_range().len(), 32 * 1024);
    let major = boot.hart();
    let minor_hart = usize::from(major == 0);
    assert!(major < 4, "unexpected QEMU Boot Hart");
    let runtime = Execution::with_minor(boot, unsafe { &mut *(&raw mut STACKS) }, &MINOR)
        .expect("Bare scalar execution");
    // The platform owns the resident objects and initializes them once.
    let execution: &'static Execution = unsafe { (&mut *(&raw mut EXECUTION)).write(runtime) };
    // The continuation holds scalar locals, the Unit stack and shared fixture
    // state across Yield. It holds no Hart-local resource.
    let a = unsafe { execution.create_migratable(first) }.expect("migratable Unit");
    let pinned = unsafe { execution.create(pinned) }.expect("create on major");
    execution.submit(pinned).expect("admit pinned Unit");
    assert!(matches!(
        unsafe { execution.start_minor(8, run_minor) },
        Err(Error::Firmware(_))
    ));
    unsafe {
        (&mut *(&raw mut SCENARIO)).write(Scenario {
            first: a,
            major,
            minor: minor_hart,
        });
        execution
            .start_minor(minor_hart, run_minor)
            .expect("start actual minor Hart");
    }
    wait(Phase::AffinityChecked);
    finish(execution, dispatch(execution, pinned, major), 17);
    execution.submit(a).expect("admit migratable Unit");

    let previous = dispatch(execution, a, major);
    unsafe { (&mut *(&raw mut PREVIOUS)).write(previous) };
    publish(Phase::FirstStopped);
    wait(Phase::WithheldChecked);
    assert_eq!(execution.pending_leave(), Some(previous));
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
    assert_eq!(limen_register_probe_migrated.load(Relaxed), 1);

    write(
        "PASS kernel-execution-harts active=2 affinity=1 pending=1 migrated=1 registers=12 stale=1 ended=3 rollback=1\n",
    );
    shutdown(true)
}

struct Console;

impl Write for Console {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        write(text);
        Ok(())
    }
}

fn observe(hart: usize) {
    let _ = writeln!(Console, "OBSERVED kernel-execution-harts hart={hart}");
}

#[unsafe(no_mangle)]
extern "C" fn limen_register_probe_mismatch(register: usize) -> ! {
    let _ = writeln!(
        Console,
        "FAIL kernel-execution-harts: register s{register} changed"
    );
    shutdown(false)
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    let _ = writeln!(Console, "FAIL kernel-execution-harts: {info}");
    shutdown(false)
}
