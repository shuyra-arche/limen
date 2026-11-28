//! Pure Sign deadlines through the original RISC-V supervisor timer.

use core::{
    arch::asm,
    cell::UnsafeCell,
    hint::{black_box, spin_loop},
    mem::MaybeUninit,
    num::NonZeroUsize,
    ops::Range,
    sync::atomic::{AtomicU64, Ordering::Relaxed},
};

use kernel::{
    basis::{
        execution::{
            Current, DeadlineError, DeadlineProgress, Error, Execution, KernelBinding, RuntimeEntry,
        },
        room::{AdmissionId, Occupancy, Participants},
        sign::Sign,
        unit::{End, Lifecycle, Stacks, UnitId, UnitRecord},
    },
    entry::boot::Boot,
    metal::{
        chrono::riscv64::{description::RiscvTimebase, time::Counter},
        interrupt::{
            riscv64::internal::{
                ipi::Receiver,
                supervisor::Reception,
                timer::{Failure, State, Timer},
            },
            treaty::{AbsoluteTimer, TimerEffect},
        },
        platform::{
            boot::treaty::write, description::treaty::devicetree::DeviceTree,
            firmware::treaty::sbi::TimerService,
        },
        processor::{
            description::riscv64::{
                cpu::RiscvCpus,
                isa::{RiscvExtension, RiscvIsa},
            },
            treaty::Dispatchers,
        },
    },
};

#[path = "native_stop.rs"]
mod native_stop;
#[path = "../../metal/interrupt/riscv64/internal/timer/reception.rs"]
mod reception;

static mut STACKS: Stacks = Stacks::new();
static mut RECORDS: [UnitRecord<KernelBinding>; 2] = [const { UnitRecord::EMPTY }; 2];
static mut PARTICIPANTS: Participants<1> = Participants::new();
static mut DISPATCHERS: Dispatchers<Occupancy, 1> = Dispatchers::new();
static mut EXECUTION: MaybeUninit<Execution> = MaybeUninit::uninit();
static mut RUNTIME: MaybeUninit<RuntimeEntry<Initial>> = MaybeUninit::uninit();
static mut TREE: [u8; 64 * 1024] = [0; 64 * 1024];
static mut COUNTER: MaybeUninit<Counter<'static>> = MaybeUninit::uninit();
static mut SIGN: MaybeUninit<Sign<'static, 2>> = MaybeUninit::uninit();
static EARLY: AtomicU64 = AtomicU64::new(0);
static LATE: AtomicU64 = AtomicU64::new(0);

const EARLY_END: u64 = 1;
const LATE_END: u64 = 2;
const BUDGET: NonZeroUsize = NonZeroUsize::MIN;

struct Initial {
    major_stack: Range<usize>,
    tree: DeviceTree<'static>,
    timer_line: UnsafeCell<Option<Reception>>,
    firmware_timer: Option<TimerService>,
    complete: fn() -> !,
}

struct Parked {
    unit: UnitId,
    admission: AdmissionId,
    leave: Occupancy,
}

pub fn check(mut boot: Boot<'_>, firmware_timer: Option<TimerService>, complete: fn() -> !) -> ! {
    let original = boot
        .device_tree_description()
        .expect("original checked DTB");
    let bytes = original.bytes();
    // The guest retains exact firmware bytes in its resident image. All of
    // its post-switch descriptions use this one checked, immutable scope.
    let storage = unsafe { &mut *(&raw mut TREE) };
    let retained = storage.get_mut(..bytes.len()).expect("QEMU DTB capacity");
    retained.copy_from_slice(bytes);
    let tree = DeviceTree::new(retained).expect("retained checked DTB");
    let major = boot.take_major().expect("original major relation");
    let timer_line = boot
        .take_reception()
        .expect("original supervisor lines")
        .timer;
    let execution = Execution::new(
        major,
        boot.terminal(),
        unsafe { &mut *(&raw mut STACKS) },
        unsafe { &mut *(&raw mut RECORDS) },
        unsafe { &mut *(&raw mut PARTICIPANTS) },
        unsafe { &mut *(&raw mut DISPATCHERS) },
    )
    .map_err(|failure| failure.reason)
    .expect("resident execution supplies");
    let execution = unsafe { (&mut *(&raw mut EXECUTION)).write(execution) };
    assert_eq!(execution.attach(), Err(Error::Unsupported));
    let initial = Initial {
        major_stack: boot.stack_range(),
        tree,
        timer_line: UnsafeCell::new(Some(timer_line)),
        firmware_timer,
        complete,
    };
    let entry = unsafe { (&mut *(&raw mut RUNTIME)).write(RuntimeEntry::new(schedule, initial)) };
    unsafe { execution.run(entry) }.expect("resident dispatcher");
}

fn schedule(initial: &'static Initial) -> ! {
    let stack: usize;
    unsafe { asm!("mv {}, sp", out(reg) stack, options(nostack, nomem)) };
    assert!(!initial.major_stack.contains(&stack));
    // The permanent entry executes once on this processor and takes the
    // exact exclusive reception line before any timer can be armed.
    let mut line = unsafe { (&mut *initial.timer_line.get()).take() }.expect("original timer line");
    let cpus = RiscvCpus::new(initial.tree).unwrap().unwrap();
    let cpu = cpus
        .cpu(line.hart())
        .unwrap()
        .expect("original major processor");
    let isa = RiscvIsa::describe(&cpu).unwrap().unwrap();
    let timebase = RiscvTimebase::describe(&cpus).unwrap().unwrap();
    let counter = unsafe { Counter::admit(timebase, line.hart()) }.expect("local time counter");
    let counter = unsafe { (&mut *(&raw mut COUNTER)).write(counter) };
    let sign = unsafe { (&mut *(&raw mut SIGN)).write(Sign::new(counter.clock())) };
    let direct = initial.firmware_timer.is_none();
    let mut timer = if let Some(service) = initial.firmware_timer {
        assert!(!isa.has(RiscvExtension::SupervisorTimer));
        unsafe { Timer::admit_sbi(counter, service, &mut line) }.unwrap()
    } else {
        unsafe { Timer::admit_sstc(&cpu, isa, counter, &mut line) }.unwrap()
    };
    let State::Inherited(comparison) = timer.state() else {
        panic!("original inherited timer state");
    };
    assert_eq!(comparison.is_some(), direct);
    clear_inherited(counter, &mut timer);
    check_comparison(&timer, None, direct);
    reception::install();

    let execution = unsafe { (&*(&raw const EXECUTION)).assume_init_ref() };
    execution.attach().expect("actual Room participation");
    let mut software = unsafe { Receiver::admit(counter.clock().processor()) }.unwrap();
    software.open().unwrap();
    assert_eq!(software.pending(), Ok(false));
    assert_eq!(execution.enter(), Err(Error::Unsupported));
    // The timer-only Hook covers no software IRQ, and no software notification
    // was issued while this line was open with global IRQ delivery masked.
    unsafe { software.shut() }.unwrap();
    let hertz = u64::from(counter.hertz());
    let later = counter.read().unwrap() + 2 * hertz;
    LATE.store(later, Relaxed);
    let late = park(execution, wait_late);
    execution.vacate(late.leave).expect("late machine leave");
    assert_eq!(execution.enter(), Ok(None));
    assert_eq!(
        service(execution, sign, &mut timer),
        DeadlineProgress::Armed
    );
    check_comparison(&timer, Some(later), direct);

    let earlier = counter.read().unwrap() + hertz / 4;
    assert!(earlier < later);
    EARLY.store(earlier, Relaxed);
    let early = park(execution, wait_early);
    assert_eq!(
        service(execution, sign, &mut timer),
        DeadlineProgress::Armed
    );
    check_comparison(&timer, Some(earlier), direct);

    while timer.counter().unwrap() < earlier {
        spin_loop();
    }
    assert_eq!(
        service(execution, sign, &mut timer),
        DeadlineProgress::AwaitInterrupt
    );
    check_comparison(&timer, Some(earlier), direct);
    reception::receive(&mut timer, later);
    assert_eq!(timer.pending_effect(), Ok(Some(TimerEffect::Clearing)));
    assert_eq!(
        service_once(execution, sign, &mut timer),
        DeadlineProgress::Recheck,
    );
    assert_eq!(execution.pending_leave(), Ok(Some(early.leave)));
    assert_eq!(execution.enter(), Err(Error::Busy));
    execution.vacate(early.leave).expect("early machine leave");
    assert_eq!(
        service(execution, sign, &mut timer),
        DeadlineProgress::Armed
    );
    check_comparison(&timer, Some(later), direct);
    finish(execution, early, EARLY_END);
    assert_eq!(execution.enter(), Ok(None));

    reception::receive(&mut timer, later + hertz);
    assert_eq!(timer.pending_effect(), Ok(Some(TimerEffect::Clearing)));
    assert_eq!(service(execution, sign, &mut timer), DeadlineProgress::Idle);
    check_comparison(&timer, None, direct);
    assert_eq!(timer.pending(), Ok(false));
    finish(execution, late, LATE_END);
    assert_eq!(execution.enter(), Ok(None));
    write("PASS sign-deadline armed=2 advanced=1 irq=2 resumed=2 leaves=4 route=");
    write(if direct { "sstc\n" } else { "sbi-time\n" });
    if direct {
        native_stop::check(execution, &mut timer);
    }
    (initial.complete)()
}

fn clear_inherited(counter: &Counter<'_>, timer: &mut Timer<'_>) {
    let limit = counter.read().unwrap() + u64::from(counter.hertz());
    let mut cleared = unsafe { timer.clear() };
    while cleared == Err(Failure::Busy) {
        assert!(counter.read().unwrap() < limit);
        assert_eq!(timer.pending_effect(), Ok(Some(TimerEffect::Clearing)));
        cleared = timer.confirm_effect();
    }
    cleared.expect("inherited timer cleared and drained");
}

fn check_comparison(timer: &Timer<'_>, expected: Option<u64>, direct: bool) {
    let state = expected.map_or(State::Idle, State::Armed);
    assert_eq!(timer.state(), state);
    assert_eq!(
        timer.confirmed_deadline().unwrap().is_some(),
        expected.is_some(),
    );
    if direct {
        let count: u64;
        unsafe { asm!("csrr {}, 0x14d", out(reg) count, options(nostack)) };
        assert_eq!(count, expected.unwrap_or(u64::MAX));
    }
}

fn service(execution: &Execution, sign: &Sign<'_, 2>, timer: &mut Timer<'_>) -> DeadlineProgress {
    let limit = timer.counter().unwrap() + u64::from(counter().hertz());
    loop {
        let progress = service_once(execution, sign, timer);
        if progress != DeadlineProgress::Recheck {
            return progress;
        }
        assert!(
            timer.counter().unwrap() < limit,
            "deadline service did not settle"
        );
        spin_loop();
    }
}

fn service_once(
    execution: &Execution,
    sign: &Sign<'_, 2>,
    timer: &mut Timer<'_>,
) -> DeadlineProgress {
    // The dispatcher retains the comparator and its masked typed reception
    // exclusively for this Sign, even while native clearing still drains.
    match unsafe { execution.service_deadlines(sign, timer, BUDGET) } {
        Err(DeadlineError::Timer(Failure::Busy))
            if timer.pending_effect() == Ok(Some(TimerEffect::Clearing)) =>
        {
            DeadlineProgress::Recheck
        }
        result => result.expect("real deadline service"),
    }
}

fn park(execution: &'static Execution, entry: fn(&mut Current) -> u64) -> Parked {
    let unit = unsafe { execution.create(entry) }.expect("fixed-local Kernel Unit");
    let admission = execution.submit(unit).unwrap();
    let leave = execution.enter().unwrap().expect("parked Kernel Unit");
    assert_eq!(leave.unit(), unit);
    assert_eq!(leave.admission(), admission);
    assert_eq!(execution.pending_leave(), Ok(Some(leave)));
    assert_eq!(execution.lifecycle(unit), Ok(Lifecycle::Live));
    Parked {
        unit,
        admission,
        leave,
    }
}

fn finish(execution: &'static Execution, parked: Parked, value: u64) {
    let returned = execution.enter().unwrap().expect("resumed original Unit");
    assert_eq!(returned.unit(), parked.unit);
    assert_eq!(returned.admission(), parked.admission);
    assert_eq!(returned.processor(), parked.leave.processor());
    assert_ne!(returned, parked.leave);
    assert_eq!(execution.vacate(parked.leave), Err(Error::Gone));
    assert_eq!(execution.pending_leave(), Ok(Some(returned)));
    execution.vacate(returned).expect("native return leave");
    execution.finish_end(parked.unit).unwrap();
    assert_eq!(execution.reap(parked.unit), Ok(End::Returned { value }));
}

fn counter() -> &'static Counter<'static> {
    // The resident dispatcher initializes this before admitting either Unit.
    unsafe { (&*(&raw const COUNTER)).assume_init_ref() }
}

fn wait_early(current: &mut Current) -> u64 {
    wait(current, EARLY.load(Relaxed), EARLY_END)
}

fn wait_late(current: &mut Current) -> u64 {
    wait(current, LATE.load(Relaxed), LATE_END)
}

fn wait(current: &mut Current, count: u64, value: u64) -> u64 {
    let counter = counter();
    let sign = unsafe { (&*(&raw const SIGN)).assume_init_ref() };
    assert_eq!(current.processor() as u64, counter.hart());
    assert!(counter.read().unwrap() < count);
    let original = black_box(value);
    let address = core::ptr::from_ref(black_box(&original)).addr();
    // The resident dispatcher services this exact Sign on a native timer
    // whose typed reception stays installed across both continuations.
    unsafe { current.wait_until(sign, counter.deadline(count)) }.expect("real deadline wait");
    assert!(counter.read().unwrap() >= count);
    assert_eq!(core::ptr::from_ref(black_box(&original)).addr(), address);
    black_box(original)
}
