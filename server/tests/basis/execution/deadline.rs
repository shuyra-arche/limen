//! Pure Sign deadlines through the original RISC-V supervisor timer.

use core::{
    hint::{black_box, spin_loop},
    mem::MaybeUninit,
    num::NonZeroUsize,
    sync::atomic::{AtomicU64, Ordering::Relaxed},
};

use kernel::{
    basis::{
        execution::{Current, DeadlineError, DeadlineProgress, Error, Execution},
        room::{AdmissionId, Occupancy},
        sign::Sign,
        unit::{End, Lifecycle, UnitId},
    },
    entry::boot::Boot,
    metal::{
        interrupt::{
            riscv64::internal::{
                ipi::Receiver,
                timer::{Failure, Timer},
            },
            treaty::{AbsoluteTimer, TimerEffect},
        },
        platform::{boot::treaty::write, firmware::treaty::sbi::TimerService},
    },
};

#[path = "native_stop.rs"]
mod native_stop;
#[path = "../../metal/interrupt/riscv64/internal/timer/reception.rs"]
mod reception;
#[path = "riscv_timer.rs"]
mod setup;

use setup::{Prepared, check_comparison, clear_inherited, counter, execution};

static mut SIGN: MaybeUninit<Sign<'static, 2>> = MaybeUninit::uninit();
static EARLY: AtomicU64 = AtomicU64::new(0);
static LATE: AtomicU64 = AtomicU64::new(0);

const EARLY_END: u64 = 1;
const LATE_END: u64 = 2;
const BUDGET: NonZeroUsize = NonZeroUsize::MIN;

struct Parked {
    unit: UnitId,
    admission: AdmissionId,
    leave: Occupancy,
}

pub fn check(boot: Boot<'_>, firmware_timer: Option<TimerService>, complete: fn() -> !) -> ! {
    setup::run(boot, firmware_timer, schedule, complete)
}

fn schedule(
    Prepared {
        mut timer,
        direct,
        complete,
    }: Prepared,
) -> ! {
    let counter = counter();
    let sign = unsafe { (&mut *(&raw mut SIGN)).write(Sign::new(counter.clock())) };
    reception::install();

    let execution = execution();
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
    complete()
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
