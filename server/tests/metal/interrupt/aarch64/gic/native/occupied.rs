//! Room dispatch across native GIC interruption and idle wake.

use core::{
    cell::UnsafeCell,
    hint::black_box,
    mem::{MaybeUninit, size_of},
    sync::atomic::{AtomicUsize, Ordering::Release},
};
use kernel::{
    basis::{
        execution::{Current, Error, Execution, KernelBinding, RuntimeEntry},
        room::{AdmissionId, Occupancy, Participants},
        unit::{End, Lifecycle, Stacks, UnitId, UnitRecord},
    },
    entry::boot::Boot,
    metal::{
        interrupt::aarch64::gic::redistributor::Frame, processor::treaty::Dispatchers,
        sync::treaty::LeafLock,
    },
};

const RETURN: u64 = 0x4b17;
const WOKEN_RETURN: u64 = 0x5a91;

static mut STACKS: Stacks = Stacks::new();
static mut RECORDS: [UnitRecord<KernelBinding>; 1] = [const { UnitRecord::EMPTY }];
static mut PARTICIPANTS: Participants<1> = Participants::new();
static mut DISPATCHERS: Dispatchers<Occupancy, 1> = Dispatchers::new();
static mut EXECUTION: MaybeUninit<Execution> = MaybeUninit::uninit();
static RUNTIME: RuntimeEntry<()> = RuntimeEntry::new(dispatch, ());
static PUBLISHED: LeafLock<Option<(UnitId, AdmissionId)>> = LeafLock::new(None);
static FINISHED: AtomicUsize = AtomicUsize::new(0);

struct LocalFrame(UnsafeCell<Option<Frame<'static>>>);

// The original minor is the sole accessor. The fixture transfers the frame
// between that processor's Unit and dispatcher only at ordinary boundaries.
unsafe impl Sync for LocalFrame {}

static FRAME: LocalFrame = LocalFrame(UnsafeCell::new(None));

pub(super) fn admit(boot: &mut Boot<'_>) {
    let major = boot.take_major().expect("original major relation");
    let execution = Execution::new(
        major,
        boot.terminal(),
        unsafe { &mut *(&raw mut STACKS) },
        unsafe { &mut *(&raw mut RECORDS) },
        unsafe { &mut *(&raw mut PARTICIPANTS) },
        unsafe { &mut *(&raw mut DISPATCHERS) },
    )
    .map_err(|failure| failure.reason)
    .expect("actual Room and Unit material");
    unsafe { (&raw mut EXECUTION).write(MaybeUninit::new(execution)) };
}

fn execution() -> &'static Execution {
    // The major publishes the initialized execution owner before PSCI starts
    // the minor. It remains resident for the duration of the native guest.
    unsafe { (&*(&raw const EXECUTION)).assume_init_ref() }
}

pub(super) fn in_interrupt() {
    assert_eq!(execution().pending_leave(), Err(Error::Protection));
    assert!(matches!(execution().observe_idle(), Err(Error::Protection)));
}

fn take_frame() -> Frame<'static> {
    unsafe { (&mut *FRAME.0.get()).take().expect("original minor frame") }
}

fn retain(frame: Frame<'static>) {
    let slot = unsafe { &mut *FRAME.0.get() };
    assert!(slot.is_none());
    *slot = Some(frame);
}

fn receive(current: &mut Current) -> u64 {
    let mark = black_box(RETURN);
    let address = black_box(&mark) as *const u64 as usize;
    let stack = (&raw const STACKS).addr();
    assert!((stack..stack + size_of::<Stacks>()).contains(&address));
    assert_eq!(execution().pending_leave(), Ok(None));
    assert!(matches!(execution().observe_idle(), Err(Error::Busy)));
    let frame = super::scenario::interrupt_unit(take_frame(), current.processor());
    assert_eq!(execution().pending_leave(), Ok(None));
    assert_eq!(execution().enter(), Err(Error::Busy));
    retain(frame);
    current.yield_now().unwrap();
    assert_eq!(execution().pending_leave(), Ok(None));
    assert_eq!(black_box(&mark) as *const u64 as usize, address);
    black_box(mark)
}

fn dispatch(_: &'static ()) -> ! {
    let execution = execution();
    execution.attach().expect("actual minor Room participation");
    let unit = unsafe { execution.create(receive) }.unwrap();
    let admission = execution.submit(unit).unwrap();
    let leaving = execution.enter().unwrap().expect("occupied Kernel Unit");
    assert_eq!(leaving.unit(), unit);
    assert_eq!(leaving.admission(), admission);
    assert_eq!(execution.pending_leave(), Ok(Some(leaving)));
    assert_eq!(execution.lifecycle(unit), Ok(Lifecycle::Live));
    execution.vacate(leaving).unwrap();

    let ended = execution.enter().unwrap().expect("original Unit resumed");
    assert_eq!(ended.unit(), unit);
    assert_eq!(ended.admission(), admission);
    assert_ne!(ended, leaving);
    assert_eq!(execution.vacate(leaving), Err(Error::Gone));
    assert_eq!(execution.pending_leave(), Ok(Some(ended)));
    execution.vacate(ended).unwrap();
    execution.finish_end(unit).unwrap();
    assert_eq!(execution.reap(unit), Ok(End::Returned { value: RETURN }));
    assert_eq!(execution.pending_leave(), Ok(None));
    assert_eq!(execution.enter(), Ok(None));

    let frame = wake_dispatcher(execution, take_frame());
    FINISHED.store(1, Release);
    super::scenario::resume_minor(frame)
}

pub(super) fn publish_ready() {
    let execution = execution();
    let unit = unsafe { execution.create_migratable(|_| WOKEN_RETURN) }.expect("peer Kernel Unit");
    let admission = execution.submit(unit).expect("peer Ready publication");
    PUBLISHED
        .acquire(|slot| assert!(slot.replace((unit, admission)).is_none()))
        .unwrap();
}

fn wake_dispatcher(execution: &'static Execution, frame: Frame<'static>) -> Frame<'static> {
    let processor = frame.affinity();
    let idle = execution
        .observe_idle()
        .unwrap()
        .expect("empty compatible Ready queue");
    assert!(!idle.changed());
    let frame = super::scenario::wake_room(frame, processor, &idle);
    assert!(idle.changed());
    assert!(execution.observe_idle().unwrap().is_none());
    let (unit, admission) = PUBLISHED
        .acquire(Option::take)
        .unwrap()
        .expect("peer's exact Unit and admission");
    let leave = execution
        .enter()
        .unwrap()
        .expect("dispatch the peer's Unit");
    assert_eq!(leave.unit(), unit);
    assert_eq!(leave.admission(), admission);
    assert_eq!(leave.processor(), processor);
    assert_eq!(execution.pending_leave(), Ok(Some(leave)));
    execution.vacate(leave).unwrap();
    execution.finish_end(unit).unwrap();
    assert_eq!(
        execution.reap(unit),
        Ok(End::Returned {
            value: WOKEN_RETURN
        })
    );
    let drained = execution
        .observe_idle()
        .unwrap()
        .expect("drained Ready queue");
    assert!(!drained.changed());
    frame
}

pub(super) fn start(frame: Frame<'static>) -> ! {
    retain(frame);
    let execution = execution();
    assert_eq!(execution.attach(), Err(Error::Unsupported));
    unsafe { execution.run(&RUNTIME) }.expect("resident minor dispatcher")
}

pub(super) fn check_departure() {
    super::scenario::await_round(&FINISHED, 1);
}
