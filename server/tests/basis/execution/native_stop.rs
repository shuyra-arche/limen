//! A real timer stop of a discardable native Kernel Unit instruction span.

use core::{
    arch::{asm, naked_asm},
    ptr,
    sync::atomic::{AtomicPtr, AtomicU8, AtomicUsize, Ordering::Relaxed},
    time::Duration,
};

use kernel::{
    basis::{
        execution::{Current, Error, Execution},
        room::Occupancy,
        unit::{End, Lifecycle},
    },
    metal::{
        interrupt::{riscv64::internal::timer::Timer, treaty::AbsoluteTimer},
        platform::boot::treaty::write,
        processor::treaty::TimerInterrupt,
    },
};

use super::{EXECUTION, check_comparison, clear_inherited, counter, reception};

const OUTSIDE: u8 = 1;
const STALE: u8 = 2;
const STOP: u8 = 3;
const RETURNED: u64 = 11;

static MODE: AtomicU8 = AtomicU8::new(0);
static STACK: AtomicUsize = AtomicUsize::new(0);
static CAPTURED: AtomicPtr<bool> = AtomicPtr::new(ptr::null_mut());
static mut EXPECTED: Option<Occupancy> = None;

pub(super) fn check(execution: &'static Execution, timer: &mut Timer<'_>) {
    let mut previous = None;
    for mode in [OUTSIDE, STALE, STOP, OUTSIDE] {
        MODE.store(mode, Relaxed);
        let unit = unsafe { execution.create(work) }.unwrap();
        let admission = execution.submit(unit).unwrap();
        let target = counter()
            .clock()
            .sample()
            .unwrap()
            .after(Duration::from_millis(20))
            .unwrap();
        timer.arm_at(target).unwrap();
        let leave = reception::capture(timer, Some(after_receipt), |captured| {
            CAPTURED.store(captured.as_ptr(), Relaxed);
            execution.enter().unwrap().unwrap()
        });
        CAPTURED.store(ptr::null_mut(), Relaxed);
        assert_eq!(leave.unit(), unit);
        assert_eq!(leave.admission(), admission);
        assert_eq!(execution.pending_leave(), Ok(Some(leave)));
        assert_eq!(execution.enter(), Err(Error::Busy));
        let status: usize;
        unsafe { asm!("csrr {}, sstatus", out(reg) status, options(nostack, nomem)) }
        assert_eq!(status & 2, 0);

        if let Some(previous) = previous {
            assert_ne!(leave, previous);
            assert_ne!(leave.unit(), previous.unit());
            assert_eq!(execution.vacate(previous), Err(Error::Gone));
        }
        previous = Some(leave);
        unsafe { (&raw mut EXPECTED).write(Some(leave)) };
        assert_eq!(
            execution.lifecycle(unit),
            Ok(if mode == STOP {
                Lifecycle::Live
            } else {
                Lifecycle::Ending
            })
        );
        assert_eq!(execution.finish_end(unit), Err(Error::Busy));
        execution.vacate(leave).unwrap();
        assert_eq!(execution.pending_leave(), Ok(None));
        execution.finish_end(unit).unwrap();
        assert_eq!(
            execution.reap(unit),
            Ok(if mode == STOP {
                End::Cancelled
            } else {
                End::Returned { value: RETURNED }
            })
        );
        assert_eq!(execution.vacate(leave), Err(Error::Gone));
        clear_inherited(counter(), timer);
        check_comparison(timer, None, true);
    }
    assert_eq!(execution.enter(), Ok(None));
    write("PASS native-stop outside=2 stale=1 cancelled=1 vacate=4 reused=1\n");
}

fn after_receipt(interrupt: TimerInterrupt<'_>) {
    let execution = unsafe { (&*(&raw const EXECUTION)).assume_init_ref() };
    let occupancy = unsafe { (&raw const EXPECTED).read() }.unwrap();
    assert_eq!(
        execution.request_stop(interrupt, occupancy),
        match MODE.load(Relaxed) {
            OUTSIDE => Err(Error::Unsupported),
            STALE => Err(Error::Gone),
            STOP => Ok(()),
            _ => unreachable!(),
        }
    );
}

unsafe extern "C" {
    static limen_test_discard_begin: u8;
    static limen_test_discard_end: u8;
}

fn work(current: &mut Current) -> u64 {
    let stack: usize;
    unsafe { asm!("mv {}, sp", out(reg) stack, options(nostack, nomem)) };
    let previous = STACK.swap(stack, Relaxed);
    assert!(previous == 0 || previous == stack);
    let code =
        (&raw const limen_test_discard_begin).addr()..(&raw const limen_test_discard_end).addr();
    // All live Unit frames own plain values and no cleanup in this native
    // span. The only unfinished timer effect belongs to the dispatcher.
    let occupancy = unsafe { current.allow_stop_in(code) }.unwrap();
    let mode = MODE.load(Relaxed);
    if mode != STALE {
        unsafe { (&raw mut EXPECTED).write(Some(occupancy)) };
    }
    let captured = CAPTURED.load(Relaxed);
    assert!(!captured.is_null());
    if mode == OUTSIDE {
        unsafe {
            asm!(
                "csrsi sstatus, 2",
                "2: lbu t0, 0({captured})",
                "beqz t0, 2b",
                "csrci sstatus, 2",
                captured = in(reg) captured,
                out("t0") _,
                options(nostack),
            );
        }
    } else {
        unsafe { discardable(captured) };
        assert_ne!(mode, STOP, "stopped Unit resumed in its original frame");
    }
    RETURNED
}

#[unsafe(naked)]
unsafe extern "C" fn discardable(captured: *const bool) {
    naked_asm!(
        ".global limen_test_discard_begin",
        "limen_test_discard_begin:",
        "csrsi sstatus, 2",
        "2: lbu t0, 0(a0)",
        "beqz t0, 2b",
        "csrci sstatus, 2",
        ".global limen_test_discard_end",
        "limen_test_discard_end:",
        "ret",
    );
}
