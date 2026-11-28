//! The original Sstc budget interrupt, stopped occupancy and saved continuation.

use core::{
    arch::{asm, naked_asm},
    hint::spin_loop,
    mem::MaybeUninit,
    sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    time::Duration,
};

use kernel::{
    basis::{
        execution::{ChargeSettlement, Current, Error, Execution, Fund, FundConfig, FundError},
        room::Occupancy,
        unit::{End, Lifecycle},
    },
    entry::{boot::Boot, hook::Binding},
    metal::{
        chrono::treaty::{LocalTiming, StopBounds, TimingBounds},
        interrupt::riscv64::internal::timer::Failure,
        platform::boot::treaty::write,
        processor::treaty::{RuntimeTrap, TimerInterrupt},
    },
};

#[path = "riscv_timer.rs"]
mod setup;

const QUOTA: Duration = Duration::from_secs(2);
const CEILING: Duration = Duration::from_millis(350);
const STOP: StopBounds = StopBounds {
    elapsed: Duration::from_millis(200),
    charged: Duration::from_millis(200),
    start: Duration::from_millis(25),
};
const RETURNED: u64 = 33;

static mut FUND: MaybeUninit<Fund<'static>> = MaybeUninit::uninit();
static FIRST_ENTRY: AtomicU64 = AtomicU64::new(0);
static FIRST_COMPARISON: AtomicU64 = AtomicU64::new(0);
static ENTRIES: AtomicUsize = AtomicUsize::new(0);
static SPAN_ENTRIES: AtomicUsize = AtomicUsize::new(0);
static INTERRUPTS: AtomicUsize = AtomicUsize::new(0);
static MAY_FINISH: AtomicBool = AtomicBool::new(false);

static HOOK: Binding<()> = Binding::new((), None, Some(timer_interrupt));
static VECTORS: RuntimeTrap = RuntimeTrap::new(HOOK.entry());

unsafe extern "C" {
    static limen_test_budget_begin: u8;
    static limen_test_budget_end: u8;
}

pub(crate) fn check(boot: Boot<'_>, complete: fn() -> !) -> ! {
    setup::run(boot, None, dispatch, complete)
}

fn dispatch(
    setup::Prepared {
        timer,
        direct,
        complete,
    }: setup::Prepared,
) -> ! {
    assert!(direct);
    let source = setup::counter();
    let clock = source.clock();
    let start = source.read().unwrap();
    let hertz = clock.frequency();
    let shuts = start + 2 * hertz.get();
    // This fixture supplies a timing profile for the original QEMU virtual
    // counter. Execution tests the actual IRQ and accounting sequence.
    let timing = unsafe {
        LocalTiming::admit(
            clock,
            TimingBounds {
                minimum_hertz: hertz,
                maximum_hertz: hertz,
                count_error: 1,
                valid: 0..=start + 8 * hertz.get(),
            },
        )
    }
    .expect("local virtual time profile");
    let configuration = FundConfig {
        opens: source.deadline(start),
        shuts: source.deadline(shuts),
        quota: QUOTA,
        ceiling: CEILING,
        stop: STOP,
    };
    // The original timer line and counter are resident. The test admits only
    // the bounded native code span on this isolated Hart under this profile.
    let owner = unsafe { Fund::new(timer, timing, configuration) }.expect("original Fund");
    let owner = unsafe { (&mut *(&raw mut FUND)).write(owner) };
    unsafe { VECTORS.install() }.expect("permanent native timer entry");
    let execution = setup::execution();
    execution.attach_fund(owner).expect("attach original Fund");
    let span =
        (&raw const limen_test_budget_begin).addr()..(&raw const limen_test_budget_end).addr();
    let unit =
        unsafe { execution.create_funded(owner, work, span.clone()) }.expect("funded native work");
    let admission = execution.submit(unit).expect("one original admission");

    let first = execution
        .enter_funded(owner)
        .unwrap()
        .expect("stopped occupancy");
    assert_eq!(first.unit(), unit);
    assert_eq!(first.admission(), admission);
    assert_eq!(execution.pending_leave(), Ok(Some(first)));
    assert_eq!(execution.lifecycle(unit), Ok(Lifecycle::Live));
    assert_eq!(execution.finish_end(unit), Err(Error::Busy));
    assert!(execution.enter_funded(owner).is_err());
    assert_eq!(execution.vacate(first), Err(Error::Unsupported));
    assert_eq!(execution.pending_leave(), Ok(Some(first)));
    assert_eq!(ENTRIES.load(Ordering::Relaxed), 1);
    assert_eq!(SPAN_ENTRIES.load(Ordering::Relaxed), 1);
    assert_eq!(INTERRUPTS.load(Ordering::Relaxed), 1);
    let origin = FIRST_ENTRY.load(Ordering::Relaxed);
    let target = FIRST_COMPARISON.load(Ordering::Relaxed);
    assert!(origin < target && target <= source.read().unwrap());
    assert!(target - origin < CEILING.as_millis() as u64 * hertz.get() / 1000);

    let first_charge = settle(execution, owner, first);
    assert_eq!(first_charge.occupancy, first);
    assert_eq!(first_charge.spent, first_charge.evidence.maximum);
    check_charge(&first_charge);
    assert_eq!(execution.pending_leave(), Ok(None));
    assert!(matches!(
        execution.vacate_funded(owner, first),
        Err(FundError::Execution(Error::Gone))
    ));

    MAY_FINISH.store(true, Ordering::Release);
    let second = execution
        .enter_funded(owner)
        .unwrap()
        .expect("resumed occupancy");
    assert_ne!(second, first);
    assert_eq!(second.unit(), unit);
    assert_eq!(second.admission(), admission);
    assert_eq!(second.processor(), first.processor());
    assert_eq!(execution.pending_leave(), Ok(Some(second)));
    assert_eq!(execution.lifecycle(unit), Ok(Lifecycle::Live));
    assert_eq!(ENTRIES.load(Ordering::Relaxed), 1);
    assert_eq!(SPAN_ENTRIES.load(Ordering::Relaxed), 1);
    assert_eq!(INTERRUPTS.load(Ordering::Relaxed), 1);
    assert!(matches!(
        execution.vacate_funded(owner, first),
        Err(FundError::Execution(Error::Gone))
    ));
    assert_eq!(execution.pending_leave(), Ok(Some(second)));
    let second_charge = settle(execution, owner, second);
    assert_eq!(second_charge.occupancy, second);
    assert_eq!(
        second_charge.spent,
        first_charge.spent + second_charge.evidence.maximum
    );
    assert!(second_charge.free < first_charge.free);
    check_charge(&second_charge);

    assert_eq!(execution.pending_leave(), Ok(None));
    assert!(execution.enter_funded(owner).unwrap().is_none());
    execution
        .finish_end(unit)
        .expect("returned continuation retired");
    assert_eq!(execution.reap(unit), Ok(End::Returned { value: RETURNED }));

    let blocked = unsafe { execution.create_funded(owner, work, span) }.unwrap();
    execution.submit(blocked).unwrap();
    let near_shut = shuts - hertz.get() / 10;
    while source.read().unwrap() < near_shut {
        spin_loop();
    }
    // This Period has remaining quota, but less elapsed time than the
    // admitted stop tail. There is no comparison that can cover a Start.
    assert!(matches!(
        execution.enter_funded(owner),
        Err(FundError::Execution(Error::Timing))
    ));
    assert_eq!(execution.pending_leave(), Ok(None));
    assert_eq!(execution.lifecycle(blocked), Ok(Lifecycle::Live));
    assert_eq!(ENTRIES.load(Ordering::Relaxed), 1);
    assert_eq!(comparison(), u64::MAX);

    write("PASS rv64-fund armed=2 stop=1 resume=1 native-meter=2 joined-leave=2 denied-start=1\n");
    complete()
}

fn settle(execution: &Execution, owner: &Fund<'_>, occupancy: Occupancy) -> ChargeSettlement {
    let source = setup::counter();
    let limit = source.read().unwrap() + source.clock().frequency().get() / 10;
    loop {
        match execution.vacate_funded(owner, occupancy) {
            Err(FundError::Timer(Failure::Busy)) => {
                assert_eq!(execution.pending_leave(), Ok(Some(occupancy)));
                assert!(
                    source.read().unwrap() < limit,
                    "native timer failed to drain"
                );
                spin_loop();
            }
            result => return result.expect("one original Charge, Meter and Leave"),
        }
    }
}

fn check_charge(charge: &ChargeSettlement) {
    assert!(Duration::ZERO < charge.evidence.minimum);
    assert!(charge.evidence.minimum <= charge.evidence.maximum);
    assert!(charge.evidence.maximum < CEILING);
    assert_eq!(charge.excess, Duration::ZERO);
    assert_eq!(charge.spent + charge.free, QUOTA);
}

fn timer_interrupt(_: &(), capture: TimerInterrupt<'_>) {
    // The original resident owner is published before installing this entry.
    let owner = unsafe { (&*(&raw const FUND)).assume_init_ref() };
    if owner
        .on_timer(setup::execution(), capture)
        .expect("original funded timer")
    {
        INTERRUPTS.fetch_add(1, Ordering::Relaxed);
    }
}

fn work(_: &mut Current) -> u64 {
    assert_eq!(ENTRIES.fetch_add(1, Ordering::Relaxed), 0);
    let counter = setup::counter();
    let entry = counter.read().unwrap();
    let comparator = comparison();
    assert_ne!(comparator, u64::MAX);
    FIRST_ENTRY.store(entry, Ordering::Relaxed);
    FIRST_COMPARISON.store(comparator, Ordering::Relaxed);
    let hertz = counter.clock().frequency().get();
    let value = unsafe {
        budget_work(
            MAY_FINISH.as_ptr(),
            entry + 4 * hertz,
            hertz / 500,
            SPAN_ENTRIES.as_ptr(),
        )
    };
    assert_eq!(value, RETURNED, "original PC, registers and stack");
    value
}

fn comparison() -> u64 {
    let comparator: u64;
    unsafe { asm!("csrr {}, 0x14d", out(reg) comparator, options(nostack, nomem)) };
    comparator
}

#[unsafe(naked)]
unsafe extern "C" fn budget_work(
    finish: *const bool,
    watchdog: u64,
    dwell: u64,
    entries: *mut usize,
) -> u64 {
    naked_asm!(
        ".global limen_test_budget_begin",
        "limen_test_budget_begin:",
        "li t0, 1",
        "amoadd.d zero, t0, (a3)",
        "addi sp, sp, -16",
        "li t2, 0x13579bdf",
        "sd t2, 8(sp)",
        "li a4, 0x2468ace0",
        "csrsi sstatus, 2",
        "2: lbu t0, 0(a0)",
        "fence r, rw",
        "bnez t0, 3f",
        "rdtime t1",
        "bltu t1, a1, 2b",
        "li a0, 0",
        "j 5f",
        "3: rdtime t0",
        "csrr t3, 0x14d",
        "li t4, -1",
        "li a0, 2",
        "beq t3, t4, 5f",
        "bgeu t0, t3, 5f",
        "add a2, a2, t0",
        "4: rdtime t0",
        "bltu t0, a2, 4b",
        "li a0, 1",
        "ld t0, 8(sp)",
        "li t1, 0x13579bdf",
        "bne t0, t1, 5f",
        "bne t2, t1, 5f",
        "li t1, 0x2468ace0",
        "bne a4, t1, 5f",
        "li a0, {returned}",
        "5: csrci sstatus, 2",
        "addi sp, sp, 16",
        ".global limen_test_budget_end",
        "limen_test_budget_end:",
        "ret",
        returned = const RETURNED,
    );
}
