#![no_std]
#![no_main]

use core::{
    arch::asm,
    hint::black_box,
    mem::{MaybeUninit, size_of},
    panic::PanicInfo,
    sync::atomic::{AtomicUsize, Ordering::Relaxed},
};

use kernel::entry::{
    boot::{Boot, shutdown, write},
    execution::{Current, End, Error, Execution, Lifecycle, Occupancy, Stacks, UnitId},
};

static mut STACKS: Stacks = Stacks::new();
static mut EXECUTION: MaybeUninit<Execution> = MaybeUninit::uninit();
static SEQUENCE: AtomicUsize = AtomicUsize::new(0);
static FIRST_STACK: AtomicUsize = AtomicUsize::new(0);
static SECOND_STACK: AtomicUsize = AtomicUsize::new(0);

fn execution() -> &'static Execution {
    // The trusted platform initializes this once before entering any Unit.
    unsafe { (&*(&raw const EXECUTION)).assume_init_ref() }
}

fn stack_address(value: &usize) -> usize {
    let address = core::ptr::from_ref(black_box(value)).addr();
    let base = (&raw const STACKS).addr();
    assert!((base..base + size_of::<Stacks>()).contains(&address));
    address
}

fn first(current: &mut Current) -> u64 {
    let local = black_box(0x1234_5678usize);
    assert_eq!(SEQUENCE.fetch_add(1, Relaxed), 0);
    FIRST_STACK.store(stack_address(&local), Relaxed);
    assert_eq!(execution().pending_leave(), None);
    assert_eq!(execution().enter(), Err(Error::Busy));
    assert_eq!(execution().finish_end(current.id()), Err(Error::Busy));
    current.yield_now().expect("cooperative leave and reentry");
    assert_eq!(SEQUENCE.fetch_add(1, Relaxed), 2);
    assert_eq!(stack_address(&local), FIRST_STACK.load(Relaxed));
    assert_eq!(black_box(local), 0x1234_5678);
    local as u64
}

fn second(_: &mut Current) -> u64 {
    let turn = SEQUENCE.fetch_add(1, Relaxed);
    let stack = stack_address(&turn);
    assert!(stack.abs_diff(FIRST_STACK.load(Relaxed)) >= 8 * 1024);
    match turn {
        1 => SECOND_STACK.store(stack, Relaxed),
        3 => assert_eq!(stack, SECOND_STACK.load(Relaxed)),
        _ => panic!("unexpected ready order"),
    }
    40 + turn as u64
}

fn abandoned(_: &mut Current) -> u64 {
    panic!("stopped dormant Unit entered");
}

fn dispatch(unit: UnitId) -> Occupancy {
    let execution = execution();
    let occupancy = execution.enter().expect("dispatch").expect("ready unit");
    assert_eq!(occupancy.unit(), unit);
    assert_eq!(execution.pending_leave(), Some(occupancy));
    occupancy
}

fn finish(unit: UnitId, occupancy: Occupancy, value: u64) {
    let execution = execution();
    assert_eq!(execution.lifecycle(unit), Ok(Lifecycle::Ending));
    assert_eq!(execution.finish_end(unit), Err(Error::Busy));
    assert_eq!(execution.reap(unit), Err(Error::State));
    assert_eq!(execution.enter(), Err(Error::Busy));
    execution.vacate(occupancy).expect("matching actual leave");
    assert_eq!(execution.pending_leave(), None);
    assert_eq!(execution.lifecycle(unit), Ok(Lifecycle::Ending));
    execution.finish_end(unit).expect("released admission");
    let end = End::Returned { value };
    assert_eq!(execution.lifecycle(unit), Ok(Lifecycle::Ended(end)));
    assert_eq!(execution.finish_end(unit), Ok(()));
    assert_eq!(execution.reap(unit), Ok(end));
    assert_eq!(execution.reap(unit), Err(Error::Gone));
}

#[unsafe(no_mangle)]
extern "Rust" fn limen_platform_ready(boot: Boot) -> ! {
    assert!(boot.hart() < 4, "unexpected QEMU boot hart");
    // The fixture owns both static objects and runs on the sole Boot Hart.
    let runtime =
        Execution::new(boot, unsafe { &mut *(&raw mut STACKS) }).expect("Bare scalar execution");
    unsafe { (&mut *(&raw mut EXECUTION)).write(runtime) };
    let execution = execution();
    let a = unsafe { execution.create(first) }.expect("first unit");
    let dormant = unsafe { execution.create(abandoned) }.expect("dormant unit");
    assert_eq!(unsafe { execution.create(second) }, Err(Error::Capacity));
    assert_eq!(execution.enter(), Ok(None));

    execution
        .stop_dormant(dormant)
        .expect("stop before admission");
    assert_eq!(execution.lifecycle(dormant), Ok(Lifecycle::Ending));
    assert_eq!(execution.stop_dormant(dormant), Ok(()));
    assert_eq!(execution.submit(dormant), Err(Error::State));
    assert_eq!(execution.reap(dormant), Err(Error::State));
    assert_eq!(unsafe { execution.create(second) }, Err(Error::Capacity));
    assert_eq!(execution.enter(), Ok(None));
    execution
        .finish_end(dormant)
        .expect("finish without admission");
    assert_eq!(
        execution.lifecycle(dormant),
        Ok(Lifecycle::Ended(End::Cancelled))
    );
    assert_eq!(execution.reap(dormant), Ok(End::Cancelled));

    let b = unsafe { execution.create(second) }.expect("reuse dormant slot");
    assert_ne!(dormant, b);
    assert_eq!(execution.stop_dormant(dormant), Err(Error::Gone));
    assert_eq!(execution.submit(dormant), Err(Error::Gone));
    execution.submit(a).expect("submit first");
    execution.submit(b).expect("submit second");
    assert_eq!(execution.submit(a), Err(Error::Busy));
    assert_eq!(execution.stop_dormant(a), Err(Error::Busy));

    // The firmware handoff left every interrupt source masked. Change only
    // SIE to make the trusted entry reject this attempt before Occupy.
    unsafe { asm!("csrsi sstatus, 2", options(nostack)) };
    let rejected = execution.enter();
    unsafe { asm!("csrci sstatus, 2", options(nostack)) };
    assert_eq!(rejected, Err(Error::Unsupported));
    assert_eq!(execution.pending_leave(), None);

    let initial = dispatch(a);
    assert_eq!(execution.lifecycle(a), Ok(Lifecycle::Live));
    execution.vacate(initial).expect("return to ready");

    let leaving = dispatch(b);
    assert_eq!(execution.vacate(initial), Err(Error::Gone));
    assert_eq!(execution.pending_leave(), Some(leaving));
    finish(b, leaving, 41);

    let replacement = unsafe { execution.create(second) }.expect("retired stack reused");
    assert_ne!(replacement, b);
    assert_eq!(execution.submit(b), Err(Error::Gone));
    assert_eq!(execution.lifecycle(b), Err(Error::Gone));
    execution.submit(replacement).expect("submit replacement");

    let resumed = dispatch(a);
    assert_ne!(resumed, initial);
    assert_eq!(execution.vacate(initial), Err(Error::Gone));
    finish(a, resumed, 0x1234_5678);
    finish(replacement, dispatch(replacement), 43);
    assert_eq!(execution.enter(), Ok(None));
    assert_eq!(SEQUENCE.load(Relaxed), 4);

    write(concat!(
        "PASS kernel-execution stacks=2 resume=1 leave=4 ended=4 reuse=1 stale=1 busy=1",
        " rejected_before_occupy=1 dormant_reaped=1\n",
    ));
    shutdown(true)
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    use core::fmt::{self, Write};

    struct Console;
    impl Write for Console {
        fn write_str(&mut self, text: &str) -> fmt::Result {
            write(text);
            Ok(())
        }
    }

    let _ = writeln!(Console, "FAIL kernel-execution: {info}");
    shutdown(false)
}
