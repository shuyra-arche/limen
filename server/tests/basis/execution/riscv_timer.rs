//! The original RISC-V counter, timer and resident test dispatcher.

use core::{arch::asm, cell::UnsafeCell, mem::MaybeUninit, ops::Range};

use kernel::{
    basis::{
        execution::{Error, Execution, KernelBinding, RuntimeEntry},
        room::{Occupancy, Participants},
        unit::{Stacks, UnitRecord},
    },
    entry::boot::Boot,
    metal::{
        chrono::riscv64::{description::RiscvTimebase, time::Counter},
        interrupt::{
            riscv64::internal::{
                supervisor::Reception,
                timer::{Failure, State, Timer},
            },
            treaty::{AbsoluteTimer, TimerEffect},
        },
        platform::{
            description::treaty::devicetree::DeviceTree, firmware::treaty::sbi::TimerService,
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

static mut STACKS: Stacks = Stacks::new();
static mut RECORDS: [UnitRecord<KernelBinding>; 2] = [const { UnitRecord::EMPTY }; 2];
static mut PARTICIPANTS: Participants<1> = Participants::new();
static mut DISPATCHERS: Dispatchers<Occupancy, 1> = Dispatchers::new();
static mut EXECUTION: MaybeUninit<Execution> = MaybeUninit::uninit();
static mut RUNTIME: MaybeUninit<RuntimeEntry<Initial>> = MaybeUninit::uninit();
static mut TREE: [u8; 64 * 1024] = [0; 64 * 1024];
static mut COUNTER: MaybeUninit<Counter<'static>> = MaybeUninit::uninit();
static mut TIMER_LINE: MaybeUninit<Reception> = MaybeUninit::uninit();

struct Initial {
    major_stack: Range<usize>,
    tree: DeviceTree<'static>,
    timer_line: UnsafeCell<Option<Reception>>,
    firmware_timer: Option<TimerService>,
    schedule: fn(Prepared) -> !,
    complete: fn() -> !,
}

pub(super) struct Prepared {
    pub(super) timer: Timer<'static>,
    pub(super) direct: bool,
    pub(super) complete: fn() -> !,
}

pub(super) fn run(
    mut boot: Boot<'_>,
    firmware_timer: Option<TimerService>,
    schedule: fn(Prepared) -> !,
    complete: fn() -> !,
) -> ! {
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
        schedule,
        complete,
    };
    let entry = unsafe { (&mut *(&raw mut RUNTIME)).write(RuntimeEntry::new(prepare, initial)) };
    unsafe { execution.run(entry) }.expect("resident dispatcher");
}

fn prepare(initial: &'static Initial) -> ! {
    let stack: usize;
    unsafe { asm!("mv {}, sp", out(reg) stack, options(nostack, nomem)) };
    assert!(!initial.major_stack.contains(&stack));
    // The permanent entry executes once on this processor and takes the
    // exact exclusive reception line before any timer can be armed.
    let line = unsafe { (&mut *initial.timer_line.get()).take() }.expect("original timer line");
    let line = unsafe { (&mut *(&raw mut TIMER_LINE)).write(line) };
    let cpus = RiscvCpus::new(initial.tree).unwrap().unwrap();
    let cpu = cpus
        .cpu(line.hart())
        .unwrap()
        .expect("original major processor");
    let isa = RiscvIsa::describe(&cpu).unwrap().unwrap();
    let timebase = RiscvTimebase::describe(&cpus).unwrap().unwrap();
    let counter = unsafe { Counter::admit(timebase, line.hart()) }.expect("local time counter");
    let counter = unsafe { (&mut *(&raw mut COUNTER)).write(counter) };
    let direct = initial.firmware_timer.is_none();
    let mut timer = if let Some(service) = initial.firmware_timer {
        assert!(!isa.has(RiscvExtension::SupervisorTimer));
        unsafe { Timer::admit_sbi(counter, service, line) }.unwrap()
    } else {
        unsafe { Timer::admit_sstc(&cpu, isa, counter, line) }.unwrap()
    };
    let State::Inherited(comparison) = timer.state() else {
        panic!("original inherited timer state");
    };
    assert_eq!(comparison.is_some(), direct);
    clear_inherited(counter, &mut timer);
    check_comparison(&timer, None, direct);
    (initial.schedule)(Prepared {
        timer,
        direct,
        complete: initial.complete,
    })
}

pub(super) fn clear_inherited(counter: &Counter<'_>, timer: &mut Timer<'_>) {
    let limit = counter.read().unwrap() + u64::from(counter.hertz());
    let mut cleared = unsafe { timer.clear() };
    while cleared == Err(Failure::Busy) {
        assert!(counter.read().unwrap() < limit);
        assert_eq!(timer.pending_effect(), Ok(Some(TimerEffect::Clearing)));
        cleared = timer.confirm_effect();
    }
    cleared.expect("inherited timer cleared and drained");
}

pub(super) fn check_comparison(timer: &Timer<'_>, expected: Option<u64>, direct: bool) {
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

pub(super) fn counter() -> &'static Counter<'static> {
    // The resident dispatcher initializes this before admitting either Unit.
    unsafe { (&*(&raw const COUNTER)).assume_init_ref() }
}

pub(super) fn execution() -> &'static Execution {
    // Boot publishes this exact owner before entering the dispatcher.
    unsafe { (&*(&raw const EXECUTION)).assume_init_ref() }
}
