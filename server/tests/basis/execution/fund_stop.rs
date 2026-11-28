//! The original Sstc budget interrupt, stopped occupancy and saved continuation.

use core::{
    arch::{asm, naked_asm},
    hint::spin_loop,
    mem::MaybeUninit,
    num::NonZeroUsize,
    sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    time::Duration,
};

use kernel::{
    basis::{
        execution::{
            ChargeSettlement, Current, DeadlineProgress, Error, Execution, Fund, FundConfig,
            FundError, PeriodKey, PeriodRecord,
        },
        room::Occupancy,
        sign::Sign,
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
static mut PERIODS: [PeriodRecord; 2] = [const { PeriodRecord::EMPTY }; 2];
static mut SIGN: MaybeUninit<Sign<'static, 2>> = MaybeUninit::uninit();
static WAIT_AT: AtomicU64 = AtomicU64::new(0);
static WAIT_SPANS: AtomicUsize = AtomicUsize::new(0);
static SHARED_SPANS: AtomicUsize = AtomicUsize::new(0);
static SHARED_COMPARISON: AtomicU64 = AtomicU64::new(0);
static SHARED_FINISH: AtomicBool = AtomicBool::new(false);
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
    let sign: &'static Sign<'static, 2> =
        unsafe { (&mut *(&raw mut SIGN)).write(Sign::new(clock)) };
    let records = unsafe { &mut *(&raw mut PERIODS) };
    let owner = unsafe { Fund::new(timer, timing, configuration, sign, records) }
        .expect("original Fund");
    let owner = unsafe { (&mut *(&raw mut FUND)).write(owner) };
    unsafe { VECTORS.install() }.expect("permanent native timer entry");
    let execution = setup::execution();
    execution.attach_fund(owner).expect("attach original Fund");
    let first_period = execution.funded_period(owner).unwrap();
    assert_eq!(first_period.window().get(), 1);
    assert_eq!(
        execution.advance_funded_period(owner, first_period),
        Err(FundError::Execution(Error::Timing)),
    );
    let span =
        (&raw const limen_test_budget_begin).addr()..(&raw const limen_test_budget_end).addr();
    let unit =
        unsafe { execution.create_funded(owner, work, span.clone()) }.expect("funded native work");
    let admission = execution.submit(unit).expect("one original admission");

    let first = execution
        .enter_funded(owner, sign)
        .unwrap()
        .expect("stopped occupancy");
    assert_eq!(first.unit(), unit);
    assert_eq!(first.admission(), admission);
    assert_eq!(execution.pending_leave(), Ok(Some(first)));
    assert_eq!(execution.lifecycle(unit), Ok(Lifecycle::Live));
    assert_eq!(execution.finish_end(unit), Err(Error::Busy));
    assert!(execution.enter_funded(owner, sign).is_err());
    assert_eq!(execution.vacate(first), Err(Error::Unsupported));
    assert_eq!(execution.pending_leave(), Ok(Some(first)));
    assert_eq!(ENTRIES.load(Ordering::Relaxed), 1);
    assert_eq!(SPAN_ENTRIES.load(Ordering::Relaxed), 1);
    assert_eq!(INTERRUPTS.load(Ordering::Relaxed), 1);
    let origin = FIRST_ENTRY.load(Ordering::Relaxed);
    let target = FIRST_COMPARISON.load(Ordering::Relaxed);
    assert!(origin < target && target <= source.read().unwrap());
    assert!(target - origin < CEILING.as_millis() as u64 * hertz.get() / 1000);

    assert_eq!(
        execution.advance_funded_period(owner, first_period),
        Err(FundError::Execution(Error::Busy)),
    );
    let first_charge = settle(execution, owner, first);
    assert_eq!(first_charge.period, first_period);
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
        .enter_funded(owner, sign)
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
    assert_eq!(second_charge.period, first_period);
    assert_eq!(second_charge.occupancy, second);
    assert_eq!(
        second_charge.spent,
        first_charge.spent + second_charge.evidence.maximum
    );
    assert!(second_charge.free < first_charge.free);
    check_charge(&second_charge);

    assert_eq!(execution.pending_leave(), Ok(None));
    assert_eq!(service(execution, owner, sign), DeadlineProgress::Idle);
    assert!(execution.enter_funded(owner, sign).unwrap().is_none());
    execution
        .finish_end(unit)
        .expect("returned continuation retired");
    assert_eq!(execution.reap(unit), Ok(End::Returned { value: RETURNED }));

    shared_deadline(execution, owner, sign, span.clone(), second_charge);
    cross_period(execution, owner, sign, span.clone(), first_period, shuts);

    let blocked = unsafe { execution.create_funded(owner, work, span) }.unwrap();
    execution.submit(blocked).unwrap();
    let near_shut = shuts + 2 * hertz.get() - hertz.get() / 10;
    while source.read().unwrap() < near_shut {
        spin_loop();
    }
    // This Period has remaining quota, but less elapsed time than the
    // admitted stop tail. There is no comparison that can cover a Start.
    assert!(matches!(
        execution.enter_funded(owner, sign),
        Err(FundError::Execution(Error::Timing))
    ));
    assert_eq!(execution.pending_leave(), Ok(None));
    assert_eq!(execution.lifecycle(blocked), Ok(Lifecycle::Live));
    assert_eq!(ENTRIES.load(Ordering::Relaxed), 1);
    assert_eq!(comparison(), u64::MAX);
    let last = execution.funded_period(owner).unwrap();
    while source.read().unwrap() < shuts + 2 * hertz.get() + hertz.get() / 100 {
        spin_loop();
    }
    let closed = execution.advance_funded_period(owner, last).unwrap();
    assert_eq!(closed.period, last);
    assert_eq!(closed.spent + closed.free, closed.quota);
    assert_eq!(execution.closed_funded_period(owner, last), Ok(closed));
    assert_eq!(
        execution.funded_period(owner),
        Err(FundError::Execution(Error::Gone)),
    );
    assert_eq!(
        execution.enter_funded(owner, sign),
        Err(FundError::Execution(Error::Gone)),
    );

    write("PASS rv64-fund budget-stop=1 shared-stop=1 sign-resume=1 idle-sign=1 cross-period=1 late-settlement=1 denied-start=1 exhausted=1\n");
    complete()
}

fn cross_period(
    execution: &'static Execution,
    owner: &'static Fund<'static>,
    sign: &'static Sign<'static, 2>,
    span: core::ops::Range<usize>,
    first: PeriodKey,
    shuts: u64,
) {
    let source = setup::counter();
    let hertz = source.clock().frequency().get();
    let deadline = shuts + hertz / 2;
    WAIT_AT.store(deadline, Ordering::Relaxed);
    let waiter = unsafe { execution.create_funded(owner, wait_for_sign, span) }.unwrap();
    let admission = execution.submit(waiter).unwrap();
    let parked = execution.enter_funded(owner, sign).unwrap().unwrap();
    assert_eq!(parked.unit(), waiter);
    assert_eq!(parked.admission(), admission);
    assert_eq!(execution.pending_leave(), Ok(Some(parked)));
    assert!(source.read().unwrap() < shuts);

    // Actual departure and Meter close have occurred, but the original
    // Room charge/leave deliberately remain unsettled across the boundary.
    while source.read().unwrap() < shuts + hertz / 100 {
        spin_loop();
    }
    assert_eq!(
        execution.advance_funded_period(owner, first),
        Err(FundError::Execution(Error::Busy)),
    );
    let old_charge = settle(execution, owner, parked);
    check_charge(&old_charge);
    assert_eq!(old_charge.period, first);
    let old = execution.advance_funded_period(owner, first).unwrap();
    assert_eq!(old.period, first);
    assert_eq!(old.quota, QUOTA);
    assert_eq!(old.spent, old_charge.spent);
    assert_eq!(old.free, old_charge.free);
    assert_eq!(old.excess, Duration::ZERO);
    let next = execution.funded_period(owner).unwrap();
    assert_eq!(next.window().get(), 2);
    assert_ne!(next, first);
    assert_eq!(
        execution.advance_funded_period(owner, first),
        Err(FundError::Execution(Error::Gone)),
    );
    assert_eq!(execution.closed_funded_period(owner, first), Ok(old));

    // The original old budget comparison may still need actual reception.
    // Advance itself cannot erase the original timer or Sign obligation.
    let progress = service(execution, owner, sign);
    if progress == DeadlineProgress::AwaitInterrupt {
        receive_idle(deadline);
        assert_eq!(service(execution, owner, sign), DeadlineProgress::Armed);
    } else {
        assert_eq!(progress, DeadlineProgress::Armed);
    }
    assert_eq!(comparison(), deadline);
    assert!(source.read().unwrap() < deadline);
    assert!(execution.enter_funded(owner, sign).unwrap().is_none());

    receive_idle(deadline + hertz);
    assert_eq!(service(execution, owner, sign), DeadlineProgress::Idle);
    let resumed = execution.enter_funded(owner, sign).unwrap().unwrap();
    assert_eq!(resumed.unit(), waiter);
    assert_eq!(resumed.admission(), admission);
    assert_ne!(resumed, parked);
    assert_eq!(
        execution.vacate_funded(owner, parked),
        Err(FundError::Execution(Error::Gone)),
    );
    let new_charge = settle(execution, owner, resumed);
    check_charge(&new_charge);
    assert_eq!(new_charge.period, next);
    assert_eq!(new_charge.spent, new_charge.evidence.maximum);
    assert_eq!(execution.closed_funded_period(owner, first), Ok(old));
    execution.finish_end(waiter).unwrap();
    assert_eq!(
        execution.reap(waiter),
        Ok(End::Returned { value: RETURNED })
    );
    assert_eq!(service(execution, owner, sign), DeadlineProgress::Idle);
}

fn service(execution: &Execution, owner: &Fund<'_>, sign: &Sign<'_, 2>) -> DeadlineProgress {
    let source = setup::counter();
    let limit = source.read().unwrap() + source.clock().frequency().get();
    loop {
        match execution.service_funded_deadlines(owner, sign, NonZeroUsize::MIN) {
            Ok(DeadlineProgress::Recheck) | Err(FundError::Timer(Failure::Busy)) => {
                assert!(
                    source.read().unwrap() < limit,
                    "original comparison did not settle"
                );
                spin_loop();
            }
            progress => return progress.expect("original Sign service"),
        }
    }
}

fn shared_deadline(
    execution: &'static Execution,
    owner: &'static Fund<'static>,
    sign: &'static Sign<'static, 2>,
    span: core::ops::Range<usize>,
    previous: ChargeSettlement,
) {
    let source = setup::counter();
    let hertz = source.clock().frequency().get();
    let wrong = Sign::<1>::new(source.clock());
    assert_eq!(
        execution.service_funded_deadlines(owner, &wrong, NonZeroUsize::MIN),
        Err(FundError::Execution(Error::Timing)),
    );

    let waiter = unsafe { execution.create_funded(owner, wait_for_sign, span.clone()) }.unwrap();
    let runner = unsafe { execution.create_funded(owner, shared_work, span.clone()) }.unwrap();
    let deadline = source.read().unwrap() + hertz / 10;
    WAIT_AT.store(deadline, Ordering::Relaxed);
    let wait_admission = execution.submit(waiter).unwrap();
    let parked = execution.enter_funded(owner, sign).unwrap().unwrap();
    assert_eq!(parked.unit(), waiter);
    assert_eq!(parked.admission(), wait_admission);
    assert_eq!(execution.pending_leave(), Ok(Some(parked)));
    assert_eq!(execution.vacate(parked), Err(Error::Unsupported));
    let wait_charge = settle(execution, owner, parked);
    check_charge(&wait_charge);
    assert_eq!(
        wait_charge.spent,
        previous.spent + wait_charge.evidence.maximum,
    );
    assert_eq!(WAIT_SPANS.load(Ordering::Relaxed), 1);

    assert_eq!(service(execution, owner, sign), DeadlineProgress::Armed);
    assert_eq!(comparison(), deadline);
    assert!(source.read().unwrap() < deadline);
    assert!(execution.enter_funded(owner, sign).unwrap().is_none());

    let runner_admission = execution.submit(runner).unwrap();
    let interrupts = INTERRUPTS.load(Ordering::Relaxed);
    let stopped = execution.enter_funded(owner, sign).unwrap().unwrap();
    assert_eq!(stopped.unit(), runner);
    assert_eq!(stopped.admission(), runner_admission);
    assert_eq!(SHARED_COMPARISON.load(Ordering::Relaxed), deadline);
    assert_eq!(comparison(), deadline);
    assert!(source.read().unwrap() >= deadline);
    assert_eq!(INTERRUPTS.load(Ordering::Relaxed), interrupts + 1);
    assert_eq!(WAIT_SPANS.load(Ordering::Relaxed), 1);
    let stopped_charge = settle(execution, owner, stopped);
    check_charge(&stopped_charge);
    assert_eq!(
        stopped_charge.spent,
        wait_charge.spent + stopped_charge.evidence.maximum,
    );
    assert_eq!(service(execution, owner, sign), DeadlineProgress::Idle);

    // The real comparison was accepted and the original charged Unit
    // left its stack before the Sign waiter could become Ready.
    SHARED_FINISH.store(true, Ordering::Release);
    let returned = execution.enter_funded(owner, sign).unwrap().unwrap();
    assert_eq!(returned.unit(), runner);
    assert_eq!(returned.admission(), runner_admission);
    assert_ne!(returned, stopped);
    assert_eq!(SHARED_SPANS.load(Ordering::Relaxed), 1);
    assert_eq!(
        execution.vacate_funded(owner, stopped),
        Err(FundError::Execution(Error::Gone)),
    );
    let returned_charge = settle(execution, owner, returned);
    check_charge(&returned_charge);
    execution.finish_end(runner).unwrap();
    assert_eq!(
        execution.reap(runner),
        Ok(End::Returned { value: RETURNED })
    );

    let resumed = execution.enter_funded(owner, sign).unwrap().unwrap();
    assert_eq!(resumed.unit(), waiter);
    assert_eq!(resumed.admission(), wait_admission);
    assert_ne!(resumed, parked);
    assert_eq!(WAIT_SPANS.load(Ordering::Relaxed), 2);
    let resumed_charge = settle(execution, owner, resumed);
    check_charge(&resumed_charge);
    assert_eq!(
        resumed_charge.spent,
        returned_charge.spent + resumed_charge.evidence.maximum,
    );
    execution.finish_end(waiter).unwrap();
    assert_eq!(
        execution.reap(waiter),
        Ok(End::Returned { value: RETURNED })
    );
    assert_eq!(service(execution, owner, sign), DeadlineProgress::Idle);

    // The same owner must also accept a native Sign IRQ while no Charge
    // is active, not require a fictitious running Unit.
    let idle_waiter = unsafe { execution.create_funded(owner, wait_for_sign, span) }.unwrap();
    let idle_deadline = source.read().unwrap() + hertz / 10;
    WAIT_AT.store(idle_deadline, Ordering::Relaxed);
    let idle_admission = execution.submit(idle_waiter).unwrap();
    let idle_parked = execution.enter_funded(owner, sign).unwrap().unwrap();
    assert_eq!(idle_parked.unit(), idle_waiter);
    check_charge(&settle(execution, owner, idle_parked));
    assert_eq!(service(execution, owner, sign), DeadlineProgress::Armed);
    assert_eq!(comparison(), idle_deadline);
    receive_idle(idle_deadline + hertz);
    assert_eq!(service(execution, owner, sign), DeadlineProgress::Idle);
    let idle_resumed = execution.enter_funded(owner, sign).unwrap().unwrap();
    assert_eq!(idle_resumed.unit(), idle_waiter);
    assert_eq!(idle_resumed.admission(), idle_admission);
    assert_ne!(idle_resumed, idle_parked);
    check_charge(&settle(execution, owner, idle_resumed));
    execution.finish_end(idle_waiter).unwrap();
    assert_eq!(
        execution.reap(idle_waiter),
        Ok(End::Returned { value: RETURNED })
    );
    assert_eq!(service(execution, owner, sign), DeadlineProgress::Idle);
}

fn receive_idle(until: u64) {
    let previous = INTERRUPTS.load(Ordering::Relaxed);
    unsafe {
        asm!(
            "csrsi sstatus, 2",
            "2: ld t0, 0({interrupts})",
            "bne t0, {previous}, 3f",
            "rdtime t1",
            "bltu t1, {until}, 2b",
            "3: csrci sstatus, 2",
            interrupts = in(reg) INTERRUPTS.as_ptr(),
            previous = in(reg) previous,
            until = in(reg) until,
            out("t0") _,
            out("t1") _,
            options(nostack),
        );
    }
    assert_eq!(INTERRUPTS.load(Ordering::Relaxed), previous + 1);
}

fn wait_for_sign(current: &mut Current) -> u64 {
    let source = setup::counter();
    let hertz = source.clock().frequency().get();
    let deadline = WAIT_AT.load(Ordering::Relaxed);
    let now = source.read().unwrap();
    assert!(now < deadline);
    let original = unsafe {
        budget_work(
            MAY_FINISH.as_ptr(),
            now + hertz,
            hertz / 500,
            WAIT_SPANS.as_ptr(),
        )
    };
    assert_eq!(original, RETURNED);
    let sign = unsafe { (&*(&raw const SIGN)).assume_init_ref() };
    // The real QEMU fixture has one processor and no competing Turn
    // access. Its masked Arm and departure are included in STOP.
    // A reached deadline completes in this original Occupancy, without
    // publishing a Tock or consuming another Charge.
    unsafe { current.wait_until(sign, source.deadline(now)) }.unwrap();
    unsafe { current.wait_until(sign, source.deadline(deadline)) }.unwrap();
    let now = source.read().unwrap();
    assert!(now >= deadline);
    unsafe {
        budget_work(
            MAY_FINISH.as_ptr(),
            now + hertz,
            hertz / 500,
            WAIT_SPANS.as_ptr(),
        )
    }
}

fn shared_work(_: &mut Current) -> u64 {
    let source = setup::counter();
    let now = source.read().unwrap();
    let hertz = source.clock().frequency().get();
    SHARED_COMPARISON.store(comparison(), Ordering::Relaxed);
    unsafe {
        budget_work(
            SHARED_FINISH.as_ptr(),
            now + hertz,
            hertz / 500,
            SHARED_SPANS.as_ptr(),
        )
    }
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
