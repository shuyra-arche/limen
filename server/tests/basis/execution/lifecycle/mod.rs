#[cfg(any(target_arch = "riscv64", target_arch = "x86_64"))]
use kernel::metal::platform::boot::treaty::write;
#[cfg(target_arch = "riscv64")]
mod admission;
#[cfg(target_arch = "riscv64")]
mod buddy;
#[cfg(target_arch = "riscv64")]
mod interrupt;
#[cfg(target_arch = "x86_64")]
#[path = "x86_interrupt.rs"]
mod interrupt;
#[cfg(target_arch = "riscv64")]
#[path = "../../../entry/boot/material/memory.rs"]
mod memory;
#[cfg(target_arch = "riscv64")]
mod pool;
#[cfg(target_arch = "riscv64")]
#[path = "../../../entry/boot/material/pool.rs"]
mod pool_material;

use core::{
    hint::black_box,
    mem::{MaybeUninit, size_of},
    sync::atomic::{AtomicUsize, Ordering::Relaxed},
};

use kernel::{
    basis::{
        execution::{Current, Error, Execution, KernelBinding},
        room::{Occupancy, Participants},
        unit::{End, Lifecycle, Stacks, UnitId, UnitRecord},
    },
    entry::boot::Boot,
};

static mut STACKS: Stacks = Stacks::new();
static mut PARTICIPANTS: Participants<1> = Participants::new();
static mut DISPATCHERS: kernel::metal::processor::treaty::Dispatchers<Occupancy, 1> =
    kernel::metal::processor::treaty::Dispatchers::new();
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
    #[cfg(target_arch = "riscv64")]
    {
        buddy::check();
        pool::check();
    }
    assert_eq!(execution().pending_leave(), Ok(None));
    assert_eq!(execution().enter(), Err(Error::Busy));
    assert_eq!(execution().finish_end(current.id()), Err(Error::Busy));
    #[cfg(any(target_arch = "riscv64", target_arch = "x86_64"))]
    {
        interrupt::receive(current.processor());
        assert_eq!(stack_address(&local), FIRST_STACK.load(Relaxed));
        assert_eq!(black_box(local), 0x1234_5678);
    }
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

pub fn check(mut boot: Boot<'_>, complete: fn() -> !) -> ! {
    #[cfg(target_arch = "riscv64")]
    pool::prepare(&boot);
    let major_stack = boot.stack_range();
    // Each executable owns its resident supplies for exactly one admission.
    let stacks = unsafe { &mut *(&raw mut STACKS) };
    let records = unsafe { &mut *(&raw mut RECORDS) };
    let participants = unsafe { &mut *(&raw mut PARTICIPANTS) };
    let dispatchers = unsafe { &mut *(&raw mut DISPATCHERS) };
    #[cfg(target_arch = "riscv64")]
    let runtime = {
        assert!(boot.processor() < 4, "unexpected QEMU boot processor");
        let failure =
            admission::reject_constructor(&mut boot, stacks, records, participants, dispatchers);
        Execution::new(
            failure.major,
            failure.terminal,
            failure.stacks,
            failure.records,
            failure.participants,
            failure.dispatchers,
        )
    };
    #[cfg(not(target_arch = "riscv64"))]
    let runtime = {
        let major = boot.take_major().expect("original native major relation");
        assert!(boot.take_major().is_none());
        Execution::new(
            major,
            boot.terminal(),
            stacks,
            records,
            participants,
            dispatchers,
        )
    };
    let runtime = runtime
        .map_err(|failure| failure.reason)
        .expect("original handoff and independent supplies");
    unsafe { (&mut *(&raw mut EXECUTION)).write(runtime) };
    let execution = execution();
    assert_eq!(execution.enter(), Err(Error::Unsupported));
    #[cfg(target_arch = "riscv64")]
    admission::reject_attach(execution);
    assert_eq!(execution.enter(), Err(Error::Unsupported));
    execution.attach().expect("attach actual boot processor");
    assert_eq!(execution.attach(), Err(Error::Busy));
    assert_eq!(execution.enter(), Err(Error::Protection));
    #[cfg(any(target_arch = "riscv64", target_arch = "x86_64"))]
    interrupt::install(&boot);
    let mut schedule = || {
        let mark = core::hint::black_box(0u8);
        let address = core::hint::black_box(&mark) as *const u8 as usize;
        assert!(!major_stack.contains(&address));
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

        #[cfg(target_arch = "riscv64")]
        admission::reject_dispatch(execution);

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
        let replacement =
            unsafe { execution.create(run_replacement) }.expect("reuse returned stack");
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

        #[cfg(target_arch = "riscv64")]
        write(concat!(
            "PASS kernel-execution stacks=2 resume=1 leave=4 ended=4 reuse=1 stale=1 busy=1",
            " rejected_before_occupy=1 dormant_reaped=1 buddy_edits=1 binding_returned=1 admission=1 occupied_irq=1\n",
        ));
        #[cfg(target_arch = "x86_64")]
        write(concat!(
            "PASS x86 kernel-execution occupied_irq=1 eoi=1 context_restored=1",
            " room_leave=4 reap=4 stale=1\n",
        ));
        complete()
    };
    unsafe { execution.run(&mut schedule) }.expect("resident dispatcher")
}
