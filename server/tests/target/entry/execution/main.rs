#![no_std]
#![no_main]

mod buddy;
#[path = "../memory.rs"]
mod memory;
mod pool;
#[path = "../pool.rs"]
mod pool_material;

use core::{
    arch::asm,
    hint::black_box,
    mem::{MaybeUninit, size_of},
    panic::PanicInfo,
    sync::atomic::{AtomicUsize, Ordering::Relaxed},
};

use kernel::{
    basis::{
        execution::{Current, Error, EstablishError, Execution, KernelBinding},
        room::Occupancy,
        unit::{End, Lifecycle, Stacks, UnitId, UnitRecord},
    },
    entry::boot::{Boot, shutdown, write},
};

static mut STACKS: Stacks = Stacks::new();
static mut RECORDS: [UnitRecord<KernelBinding>; 4] = [const { UnitRecord::EMPTY }; 4];
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
    buddy::check();
    pool::check();
    assert_eq!(execution().pending_leave(), Ok(None));
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
    assert_eq!(turn, 1);
    let stack = stack_address(&turn);
    assert!(stack.abs_diff(FIRST_STACK.load(Relaxed)) >= 8 * 1024);
    SECOND_STACK.store(stack, Relaxed);
    41
}

fn run_replacement(_: &mut Current) -> u64 {
    let turn = SEQUENCE.fetch_add(1, Relaxed);
    assert_eq!(turn, 3);
    let stack = stack_address(&turn);
    assert!(stack.abs_diff(FIRST_STACK.load(Relaxed)) < 8 * 1024);
    assert!(stack.abs_diff(SECOND_STACK.load(Relaxed)) >= 8 * 1024);
    43
}

fn abandoned(_: &mut Current) -> u64 {
    panic!("stopped dormant Unit entered");
}

fn dispatch(unit: UnitId) -> Occupancy {
    let execution = execution();
    let occupancy = execution.enter().expect("dispatch").expect("ready unit");
    assert_eq!(occupancy.unit(), unit);
    assert_eq!(execution.pending_leave(), Ok(Some(occupancy)));
    occupancy
}

fn finish(unit: UnitId, occupancy: Occupancy, value: u64) {
    let execution = execution();
    assert_eq!(execution.lifecycle(unit), Ok(Lifecycle::Ending));
    assert_eq!(execution.finish_end(unit), Err(Error::Busy));
    assert_eq!(execution.reap(unit), Err(Error::UnitNotEnded));
    assert_eq!(execution.enter(), Err(Error::Busy));
    execution.vacate(occupancy).expect("matching actual leave");
    assert_eq!(execution.pending_leave(), Ok(None));
    assert_eq!(execution.lifecycle(unit), Ok(Lifecycle::Ending));
    execution.finish_end(unit).expect("released admission");
    let end = End::Returned { value };
    assert_eq!(execution.lifecycle(unit), Ok(Lifecycle::Ended(end)));
    assert_eq!(execution.finish_end(unit), Ok(()));
}

#[unsafe(no_mangle)]
extern "Rust" fn limen_platform_ready(boot: Boot) -> ! {
    pool::prepare(&boot);
    let major = boot.hart();
    let device_tree = boot.device_tree();
    assert!(major < 4, "unexpected QEMU boot hart");
    // The fixture owns both static objects and runs on the sole Boot Hart.
    let stacks = &raw mut STACKS;
    // Individual interrupt sources remain masked. SIE makes the execution
    // environment unsupported before the constructor binds the stack supply.
    unsafe { asm!("csrsi sstatus, 2", options(nostack)) };
    let rejected = Execution::new(boot, unsafe { &mut *stacks }, unsafe {
        &mut *(&raw mut RECORDS)
    });
    unsafe { asm!("csrci sstatus, 2", options(nostack)) };
    let Err(failure) = rejected else {
        panic!("execution admitted with interrupts enabled");
    };
    assert_eq!(failure.reason, EstablishError::Unsupported);
    assert_eq!(failure.boot.hart(), major);
    assert_eq!(failure.boot.device_tree(), device_tree);
    assert!(core::ptr::eq(failure.stacks, stacks));
    let runtime = Execution::new(failure.boot, failure.stacks, failure.records)
        .map_err(|failure| failure.reason)
        .expect("original handoff and stack supply can be retried");
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
    assert_eq!(execution.submit(dormant), Err(Error::UnitNotLive));
    assert_eq!(execution.reap(dormant), Err(Error::UnitNotEnded));
    assert_eq!(unsafe { execution.create(second) }, Err(Error::Capacity));
    assert_eq!(execution.enter(), Ok(None));
    execution
        .finish_end(dormant)
        .expect("finish without admission");
    assert_eq!(
        execution.lifecycle(dormant),
        Ok(Lifecycle::Ended(End::Cancelled))
    );
    // The original result remains live, but its stack is already reusable.
    let b = unsafe { execution.create(second) }.expect("reuse stack before Reap");
    assert_eq!(
        execution.lifecycle(dormant),
        Ok(Lifecycle::Ended(End::Cancelled))
    );
    assert_eq!(execution.reap(dormant), Ok(End::Cancelled));
    assert_ne!(dormant, b);
    assert_eq!(execution.stop_dormant(dormant), Err(Error::Gone));
    assert_eq!(execution.submit(dormant), Err(Error::Gone));
    let a_admission = execution.submit(a).expect("submit first");
    let b_admission = execution.submit(b).expect("submit second");
    assert_ne!(a_admission, b_admission);
    assert_eq!(execution.submit(a), Err(Error::Busy));
    assert_eq!(execution.stop_dormant(a), Err(Error::Busy));

    // The firmware handoff left every interrupt source masked. Change only
    // SIE to make the trusted entry reject this attempt before Occupy.
    unsafe { asm!("csrsi sstatus, 2", options(nostack)) };
    let rejected = execution.enter();
    unsafe { asm!("csrci sstatus, 2", options(nostack)) };
    assert_eq!(rejected, Err(Error::Unsupported));
    assert_eq!(execution.pending_leave(), Ok(None));

    // Change only the actual FS field: the integer-only kernel target
    // executes no FP instructions, so no extended state is consumed.
    let mut observed: usize;
    unsafe {
        asm!(
            "csrs sstatus, {initial}",
            "csrr {observed}, sstatus",
            initial = in(reg) 1usize << 13,
            observed = out(reg) observed,
            options(nostack),
        );
    }
    let rejected = execution.enter();
    unsafe { asm!("csrc sstatus, {}", in(reg) 3usize << 13, options(nostack)) };
    assert_ne!(observed & (3 << 13), 0, "QEMU exposes the actual FS field");
    assert_eq!(rejected, Err(Error::Unsupported));
    assert_eq!(execution.pending_leave(), Ok(None));

    // Global SIE is still off. A single locally enabled source is a
    // different admission failure and must not be mistaken for global SIE.
    unsafe {
        asm!(
            "csrs sie, {timer}",
            "csrr {observed}, sie",
            timer = in(reg) 1usize << 5,
            observed = out(reg) observed,
            options(nostack),
        );
    }
    let rejected = execution.enter();
    unsafe { asm!("csrc sie, {}", in(reg) 1usize << 5, options(nostack)) };
    assert_ne!(observed & (1 << 5), 0);
    assert_eq!(rejected, Err(Error::Unsupported));
    assert_eq!(execution.pending_leave(), Ok(None));
    write("PASS native-control fs=1 source-mask=1 restored=1\n");

    let initial = dispatch(a);
    assert_eq!(initial.admission(), a_admission);
    assert_eq!(execution.lifecycle(a), Ok(Lifecycle::Live));
    execution.vacate(initial).expect("return to ready");

    let leaving = dispatch(b);
    assert_eq!(leaving.admission(), b_admission);
    assert_eq!(execution.vacate(initial), Err(Error::Gone));
    assert_eq!(execution.pending_leave(), Ok(Some(leaving)));
    finish(b, leaving, 41);

    let resumed = dispatch(a);
    assert_ne!(resumed, initial);
    assert_eq!(resumed.admission(), a_admission);
    assert_eq!(execution.vacate(initial), Err(Error::Gone));
    finish(a, resumed, 0x1234_5678);
    assert_eq!(execution.reap(b), Ok(End::Returned { value: 41 }));
    assert_eq!(execution.reap(b), Err(Error::Gone));
    assert_eq!(execution.submit(b), Err(Error::Gone));

    // Only b's Unit slot is vacant. a's End remains observable while its
    // returned stack is selected for the replacement.
    let replacement = unsafe { execution.create(run_replacement) }.expect("reuse returned stack");
    assert_ne!(replacement, b);
    let replacement_admission = execution.submit(replacement).expect("submit replacement");
    assert_ne!(replacement_admission, a_admission);
    assert_ne!(replacement_admission, b_admission);
    let replaced = dispatch(replacement);
    assert_eq!(replaced.admission(), replacement_admission);
    assert_eq!(execution.vacate(leaving), Err(Error::Gone));
    assert_eq!(
        execution.lifecycle(a),
        Ok(Lifecycle::Ended(End::Returned { value: 0x1234_5678 }))
    );
    finish(replacement, replaced, 43);
    assert_eq!(execution.reap(a), Ok(End::Returned { value: 0x1234_5678 }));
    assert_eq!(execution.reap(replacement), Ok(End::Returned { value: 43 }));
    assert_eq!(execution.enter(), Ok(None));
    assert_eq!(SEQUENCE.load(Relaxed), 4);

    write(concat!(
        "PASS kernel-execution stacks=2 resume=1 leave=4 ended=4 reuse=1 stale=1 busy=1",
        " rejected_before_occupy=1 dormant_reaped=1 buddy_edits=1 binding_returned=1 admission=1\n",
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
