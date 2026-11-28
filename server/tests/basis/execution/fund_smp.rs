//! Independent funded native stops on two actual processors of one Room.

use core::{
    arch::{asm, naked_asm},
    cell::UnsafeCell,
    hint::spin_loop,
    mem::MaybeUninit,
    num::NonZeroUsize,
    sync::atomic::{
        AtomicUsize,
        Ordering::{Acquire, Relaxed, Release},
    },
    time::Duration,
};

use kernel::{
    basis::{
        execution::{
            ChargeSettlement, Current, DeadlineProgress, Error, Execution, Fund, FundConfig,
            FundError, KernelBinding, PeriodRecord, RuntimeEntry,
        },
        room::{Occupancy, Participants},
        sign::Sign,
        unit::{End, Stacks, UnitRecord},
    },
    entry::{boot::Boot, hook::Binding},
    metal::{
        chrono::{
            riscv64::{description::RiscvTimebase, time::Counter},
            treaty::{LocalTiming, StopBounds, TimingBounds},
        },
        interrupt::{
            riscv64::internal::{
                supervisor::{Error as ReceptionError, Reception, Supervisor},
                timer::{Failure, Timer},
            },
            treaty::{AbsoluteTimer, TimerEffect},
        },
        platform::{boot::treaty::write, description::treaty::devicetree::DeviceTree},
        processor::{
            description::riscv64::{cpu::RiscvCpus, isa::RiscvIsa},
            treaty::{Dispatchers, LocalStorage, Minor, RuntimeTrap, Stack, Startup, TimerInterrupt},
        },
        sync::treaty::Supply,
    },
};

const CEILING: Duration = Duration::from_millis(1_200);
const STOP: StopBounds = StopBounds {
    elapsed: Duration::from_millis(500),
    charged: Duration::from_millis(500),
    start: Duration::from_millis(50),
};
const RETURNED: u64 = 47;

struct Local {
    processor: AtomicUsize,
    line: UnsafeCell<MaybeUninit<Reception>>,
    counter: UnsafeCell<MaybeUninit<Counter<'static>>>,
    sign: UnsafeCell<MaybeUninit<Sign<'static, 1>>>,
    fund: UnsafeCell<MaybeUninit<Fund<'static>>>,
    periods: UnsafeCell<[PeriodRecord; 1]>,
    first: UnsafeCell<MaybeUninit<Occupancy>>,
    ready: AtomicUsize,
    entered: AtomicUsize,
    overlap: AtomicUsize,
    spans: AtomicUsize,
    interrupts: AtomicUsize,
    stopped: AtomicUsize,
    finish: AtomicUsize,
    done: AtomicUsize,
}

// Each material is initialized and used only on its original processor.
// The other processor may read the immutable published references and
// exact stopped Occupancy after the corresponding release publication.
unsafe impl Sync for Local {}

impl Local {
    const fn new() -> Self {
        Self {
            processor: AtomicUsize::new(usize::MAX),
            line: UnsafeCell::new(MaybeUninit::uninit()),
            counter: UnsafeCell::new(MaybeUninit::uninit()),
            sign: UnsafeCell::new(MaybeUninit::uninit()),
            fund: UnsafeCell::new(MaybeUninit::uninit()),
            periods: UnsafeCell::new([PeriodRecord::EMPTY]),
            first: UnsafeCell::new(MaybeUninit::uninit()),
            ready: AtomicUsize::new(0),
            entered: AtomicUsize::new(0),
            overlap: AtomicUsize::new(0),
            spans: AtomicUsize::new(0),
            interrupts: AtomicUsize::new(0),
            stopped: AtomicUsize::new(0),
            finish: AtomicUsize::new(0),
            done: AtomicUsize::new(0),
        }
    }

    fn fund(&'static self) -> &'static Fund<'static> {
        assert_eq!(self.ready.load(Acquire), 1);
        unsafe { (&*self.fund.get()).assume_init_ref() }
    }

    fn sign(&'static self) -> &'static Sign<'static, 1> {
        assert_eq!(self.ready.load(Acquire), 1);
        unsafe { (&*self.sign.get()).assume_init_ref() }
    }

    fn counter(&'static self) -> &'static Counter<'static> {
        assert_eq!(self.ready.load(Acquire), 1);
        unsafe { (&*self.counter.get()).assume_init_ref() }
    }

    fn first(&self) -> Occupancy {
        assert_eq!(self.stopped.load(Acquire), 1);
        unsafe { *(&*self.first.get()).assume_init_ref() }
    }
}

static LOCALS: [Local; 2] = [const { Local::new() }; 2];
static HOOKS: [Binding<usize>; 2] = [
    Binding::new(0, None, Some(interrupt)),
    Binding::new(1, None, Some(interrupt)),
];
static VECTORS: [RuntimeTrap; 2] = [
    RuntimeTrap::new(HOOKS[0].entry()),
    RuntimeTrap::new(HOOKS[1].entry()),
];
static MINOR_STACK: Stack<{ 32 * 1024 }> = Stack::new();
static MINOR_LOCAL: LocalStorage<5> = LocalStorage::new(Supply::new());
static MINOR: Minor = Minor::new(&MINOR_STACK, &MINOR_LOCAL);

static mut TREE: [u8; 64 * 1024] = [0; 64 * 1024];
static mut DESCRIPTION: MaybeUninit<DeviceTree<'static>> = MaybeUninit::uninit();
static mut STACKS: Stacks = Stacks::new();
static mut RECORDS: [UnitRecord<KernelBinding>; 2] = [const { UnitRecord::EMPTY }; 2];
static mut PARTICIPANTS: Participants<2> = Participants::new();
static mut DISPATCHERS: Dispatchers<Occupancy, 2> = Dispatchers::new();
static mut EXECUTION: MaybeUninit<Execution> = MaybeUninit::uninit();
static mut MAJOR_RUNTIME: MaybeUninit<RuntimeEntry<fn() -> !>> = MaybeUninit::uninit();
static MINOR_RUNTIME: RuntimeEntry<()> = RuntimeEntry::new(schedule_minor, ());

unsafe extern "C" {
    static limen_test_shared_fund_begin: u8;
    static limen_test_shared_fund_end: u8;
}

pub(crate) fn check(mut boot: Boot<'_>, complete: fn() -> !) -> ! {
    let major = boot.processor();
    let peer = (0..2)
        .filter(|processor| *processor != major)
        .find(|processor| {
            boot.riscv_cpus()
                .cpu(*processor as u64)
                .unwrap()
                .is_some_and(|cpu| cpu.enabled())
        })
        .expect("actual second processor");
    LOCALS[0].processor.store(major, Relaxed);
    LOCALS[1].processor.store(peer, Relaxed);
    let description = boot.device_tree_description().unwrap();
    let bytes = description.bytes();
    let tree = unsafe { &mut *(&raw mut TREE) };
    let retained = tree.get_mut(..bytes.len()).expect("firmware extent");
    retained.copy_from_slice(bytes);
    let retained = DeviceTree::new(retained).unwrap();
    unsafe { (&mut *(&raw mut DESCRIPTION)).write(retained) };

    let line = boot.take_reception().expect("original major delivery").timer;
    assert!(matches!(Supervisor::take(), Err(ReceptionError::Busy)));
    unsafe { (&mut *LOCALS[0].line.get()).write(line) };
    let execution = Execution::new(
        boot.take_major().unwrap(),
        boot.terminal(),
        unsafe { &mut *(&raw mut STACKS) },
        unsafe { &mut *(&raw mut RECORDS) },
        unsafe { &mut *(&raw mut PARTICIPANTS) },
        unsafe { &mut *(&raw mut DISPATCHERS) },
    )
    .map_err(|failure| failure.reason)
    .unwrap();
    let execution = unsafe { (&mut *(&raw mut EXECUTION)).write(execution) };
    let mut startup = Startup::Sbi(
        boot.sbi_services()
            .hart_state_management()
            .unwrap()
            .unwrap(),
    );
    unsafe { MINOR.start(peer, minor, 1, &mut startup) }.unwrap();
    let runtime = unsafe {
        (&mut *(&raw mut MAJOR_RUNTIME)).write(RuntimeEntry::new(schedule_major, complete))
    };
    unsafe { execution.run(runtime) }.expect("major dispatcher")
}

fn execution() -> &'static Execution {
    unsafe { (&*(&raw const EXECUTION)).assume_init_ref() }
}

fn minor(processor: usize, index: usize) -> ! {
    assert_eq!(index, 1);
    assert_eq!(LOCALS[1].processor.load(Relaxed), processor);
    let line = Supervisor::take().expect("original minor reception").timer;
    assert_eq!(line.hart(), processor as u64);
    assert!(matches!(Supervisor::take(), Err(ReceptionError::Busy)));
    unsafe { (&mut *LOCALS[1].line.get()).write(line) };
    unsafe { execution().run(&MINOR_RUNTIME) }.expect("minor dispatcher")
}

fn schedule_major(complete: &'static fn() -> !) -> ! {
    schedule(0);
    await_flag(LOCALS[0].counter(), &LOCALS[1].done);
    write(
        "PASS rv64-fund-smp processors=2 overlapping=2 native-stops=2 exact-accounts=4 cross-rejected=2 masked-wait-denied=2\n",
    );
    complete()
}

fn schedule_minor(_: &'static ()) -> ! {
    schedule(1);
    loop {
        spin_loop();
    }
}

fn quota(index: usize) -> Duration {
    Duration::from_secs(4 + index as u64)
}

fn prepare(index: usize) {
    let local = &LOCALS[index];
    let hart = local.processor.load(Relaxed) as u64;
    let tree = unsafe { *(&*(&raw const DESCRIPTION)).assume_init_ref() };
    let cpus = RiscvCpus::new(tree).unwrap().unwrap();
    let cpu = cpus.cpu(hart).unwrap().unwrap();
    let isa = RiscvIsa::describe(&cpu).unwrap().unwrap();
    let timebase = RiscvTimebase::describe(&cpus).unwrap().unwrap();
    let counter = unsafe { Counter::admit(timebase, hart) }.unwrap();
    let counter = unsafe { (&mut *local.counter.get()).write(counter) };
    let line = unsafe { (&mut *local.line.get()).assume_init_mut() };
    assert_eq!(line.hart(), hart);
    let mut timer = unsafe { Timer::admit_sstc(&cpu, isa, counter, line) }.unwrap();
    let now = counter.read().unwrap();
    let hertz = u64::from(counter.hertz());
    loop {
        let result = if timer.pending_effect().unwrap() == Some(TimerEffect::Clearing) {
            timer.confirm_effect()
        } else {
            unsafe { timer.clear() }
        };
        match result {
            Err(Failure::Busy) if counter.read().unwrap() < now + hertz => continue,
            result => result.unwrap(),
        }
        break;
    }
    let clock = counter.clock();
    let frequency = clock.frequency();
    let timing = unsafe {
        LocalTiming::admit(
            clock,
            TimingBounds {
                minimum_hertz: frequency,
                maximum_hertz: frequency,
                count_error: 1,
                valid: 0..=now + 12 * hertz,
            },
        )
    }
    .unwrap();
    let config = FundConfig {
        opens: counter.deadline(now),
        shuts: counter.deadline(now + 8 * hertz),
        quota: quota(index),
        ceiling: CEILING,
        stop: STOP,
    };
    let sign = unsafe { (&mut *local.sign.get()).write(Sign::new(clock)) };
    let periods = unsafe { &mut *local.periods.get() };
    let fund = unsafe { Fund::new(timer, timing, config, sign, periods) }.unwrap();
    let fund = unsafe { (&mut *local.fund.get()).write(fund) };
    unsafe { VECTORS[index].install() }.unwrap();
    execution().attach_fund(fund).unwrap();
    local.ready.store(1, Release);
}

fn await_flag(counter: &Counter<'_>, flag: &AtomicUsize) {
    let limit = counter.read().unwrap() + 5 * u64::from(counter.hertz());
    while flag.load(Acquire) == 0 {
        assert!(counter.read().unwrap() < limit, "peer did not publish");
        spin_loop();
    }
}

fn schedule(index: usize) {
    prepare(index);
    let local = &LOCALS[index];
    let other = &LOCALS[1 - index];
    let execution = execution();
    let fund = local.fund();
    let sign = local.sign();
    let counter = local.counter();
    await_flag(counter, &other.ready);
    assert_eq!(
        execution.funded_period(other.fund()),
        Err(FundError::Execution(Error::Protection)),
    );
    assert_eq!(
        execution.service_funded_deadlines(fund, other.sign(), NonZeroUsize::MIN),
        Err(FundError::Execution(Error::Timing)),
    );
    let period = execution.funded_period(fund).unwrap();
    let span = (&raw const limen_test_shared_fund_begin).addr()
        ..(&raw const limen_test_shared_fund_end).addr();
    let unit = unsafe { execution.create_funded(fund, work, span) }.unwrap();
    let admission = execution.submit(unit).unwrap();

    let first = execution.enter_funded(fund, sign).unwrap().unwrap();
    assert_eq!(first.unit(), unit);
    assert_eq!(first.admission(), admission);
    assert_eq!(first.processor(), local.processor.load(Relaxed));
    assert_eq!(execution.pending_leave(), Ok(Some(first)));
    assert_eq!(local.overlap.load(Acquire), 1);
    assert_eq!(local.spans.load(Relaxed), 1);
    assert_eq!(local.interrupts.load(Relaxed), 1);
    unsafe { (&mut *local.first.get()).write(first) };
    local.stopped.store(1, Release);
    await_flag(counter, &other.stopped);
    assert_eq!(
        execution.vacate_funded(fund, other.first()),
        Err(FundError::Execution(Error::Gone)),
    );
    assert_eq!(
        execution.vacate_funded(other.fund(), first),
        Err(FundError::Execution(Error::Protection)),
    );
    assert_eq!(
        execution.advance_funded_period(fund, period),
        Err(FundError::Execution(Error::Busy)),
    );
    let initial = settle(execution, fund, first, counter);
    assert_eq!(initial.period, period);
    assert_eq!(initial.spent, initial.evidence.maximum);
    assert_eq!(initial.spent + initial.free, quota(index));
    assert_eq!(initial.excess, Duration::ZERO);
    assert!(initial.spent > Duration::ZERO);
    local.finish.store(1, Release);

    let next = execution.enter_funded(fund, sign).unwrap().unwrap();
    assert_eq!(next.unit(), unit);
    assert_eq!(next.admission(), admission);
    assert_eq!(next.processor(), first.processor());
    assert_ne!(next, first);
    assert_eq!(local.spans.load(Relaxed), 1);
    assert_eq!(local.interrupts.load(Relaxed), 1);
    assert_eq!(
        execution.vacate_funded(fund, first),
        Err(FundError::Execution(Error::Gone)),
    );
    let closed = settle(execution, fund, next, counter);
    assert_eq!(closed.period, period);
    assert_eq!(closed.spent, initial.spent + closed.evidence.maximum);
    assert_eq!(closed.spent + closed.free, quota(index));
    assert_eq!(closed.excess, Duration::ZERO);
    execution.finish_end(unit).unwrap();
    assert_eq!(
        execution.reap(unit),
        Ok(End::Returned { value: RETURNED })
    );
    assert_eq!(
        execution.service_funded_deadlines(fund, sign, NonZeroUsize::MIN),
        Ok(DeadlineProgress::Idle),
    );
    local.done.store(1, Release);
}

fn settle(
    execution: &Execution,
    fund: &Fund<'_>,
    occupancy: Occupancy,
    counter: &Counter<'_>,
) -> ChargeSettlement {
    let limit = counter.read().unwrap() + u64::from(counter.hertz());
    loop {
        match execution.vacate_funded(fund, occupancy) {
            Err(FundError::Timer(Failure::Busy)) => {
                assert!(counter.read().unwrap() < limit, "original timer did not drain");
                spin_loop();
            }
            result => return result.unwrap(),
        }
    }
}

fn interrupt(index: &usize, capture: TimerInterrupt<'_>) {
    let local = &LOCALS[*index];
    assert_eq!(capture.processor().get(), local.processor.load(Relaxed));
    if local.fund().on_timer(execution(), capture).unwrap() {
        local.interrupts.fetch_add(1, Relaxed);
    }
}

fn work(current: &mut Current) -> u64 {
    let index = LOCALS
        .iter()
        .position(|local| local.processor.load(Relaxed) == current.processor())
        .unwrap();
    let local = &LOCALS[index];
    let other = &LOCALS[1 - index];
    let counter = local.counter();
    let now = counter.read().unwrap();
    let hertz = u64::from(counter.hertz());
    // Shared execution has no admitted bound for the masked Turn path.
    // It must reject Wait before attempting to acquire any ordinary lock.
    assert_eq!(
        unsafe { current.wait_until(local.sign(), counter.deadline(now + hertz)) },
        Err(Error::Unsupported),
    );
    let value = unsafe {
        native_work(
            other.entered.as_ptr(),
            local.entered.as_ptr(),
            local.finish.as_ptr(),
            now + 4 * hertz,
            local.overlap.as_ptr(),
            local.spans.as_ptr(),
        )
    };
    assert_eq!(value, RETURNED, "original native continuation");
    value
}

#[unsafe(naked)]
unsafe extern "C" fn native_work(
    other: *const usize,
    entered: *mut usize,
    finish: *const usize,
    watchdog: u64,
    overlap: *mut usize,
    spans: *mut usize,
) -> u64 {
    naked_asm!(
        ".global limen_test_shared_fund_begin",
        "limen_test_shared_fund_begin:",
        "addi sp, sp, -16",
        "li t2, 0x13579bdf",
        "sd t2, 8(sp)",
        "li a6, 0x2468ace0",
        "li t0, 1",
        "amoadd.d zero, t0, (a5)",
        "amoswap.d.rl zero, t0, (a1)",
        "csrsi sstatus, 2",
        "2: ld t0, 0(a0)",
        "fence r, rw",
        "beqz t0, 3f",
        "li t0, 1",
        "amoswap.d.rl zero, t0, (a4)",
        "3: ld t0, 0(a2)",
        "fence r, rw",
        "bnez t0, 4f",
        "rdtime t0",
        "bltu t0, a3, 2b",
        "li a0, 0",
        "j 5f",
        "4: li a0, 1",
        "ld t0, 8(sp)",
        "li t1, 0x13579bdf",
        "bne t0, t1, 5f",
        "bne t2, t1, 5f",
        "li t1, 0x2468ace0",
        "bne a6, t1, 5f",
        "li a0, {returned}",
        "5: csrci sstatus, 2",
        "addi sp, sp, 16",
        ".global limen_test_shared_fund_end",
        "limen_test_shared_fund_end:",
        "ret",
        returned = const RETURNED,
    );
}
