//! Absolute local Sign deadlines with HPET as the actual reference and
//! the original relative xAPIC timer as a native reminder.

use core::{
    arch::asm,
    cell::RefCell,
    hint::black_box,
    mem::MaybeUninit,
    num::NonZeroUsize,
    ops::Range,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};
use kernel::{
    basis::{
        execution::{Current, DeadlineProgress, Error, Execution, KernelBinding, RuntimeEntry},
        room::{AdmissionId, Occupancy, Participants},
        sign::Sign,
        unit::{End, Lifecycle, Stacks, UnitId, UnitRecord},
    },
    entry::{boot::Boot, hook::Binding},
    metal::{
        chrono::x86_64::hpet::Hpet,
        interrupt::x86_64::internal::apic::{CountdownTimer, LocalApic},
        memory::access::treaty::device::DeviceWindow,
        platform::boot::treaty::write,
        processor::treaty::{Dispatchers, Irq, RuntimeTrap},
    },
};
use limen_treaty::model::InternalAddress;
use uefi_fixture::q35;

#[path = "../../metal/interrupt/x86_64/prepare.rs"]
mod hardware;

const VECTOR: u8 = 0xf2;
const EARLY: Duration = Duration::from_millis(200);
const LATE: Duration = Duration::from_secs(2);
const EARLY_END: u64 = 1;
const LATE_END: u64 = 2;

static mut STACKS: Stacks = Stacks::new();
static mut RECORDS: [UnitRecord<KernelBinding>; 2] = [const { UnitRecord::EMPTY }; 2];
static mut PARTICIPANTS: Participants<1> = Participants::new();
static mut DISPATCHERS: Dispatchers<Occupancy, 1> = Dispatchers::new();
static mut EXECUTION: MaybeUninit<Execution> = MaybeUninit::uninit();
static mut RUNTIME: MaybeUninit<RuntimeEntry<Initial>> = MaybeUninit::uninit();
static mut HPET: MaybeUninit<Hpet> = MaybeUninit::uninit();
static mut SIGN: MaybeUninit<Sign<'static, 2>> = MaybeUninit::uninit();

static TRAP: RuntimeTrap = RuntimeTrap::new(VECTOR, RECEPTION.entry());
static RECEPTION: Binding<Receiver> = Binding::new(
    Receiver {
        timer: RefCell::new(None),
        captured: AtomicUsize::new(0),
    },
    interrupt,
);

struct Receiver {
    timer: RefCell<Option<CountdownTimer<'static, 'static>>>,
    captured: AtomicUsize,
}

// The original processor owns this controller. IF is masked until every
// ordinary borrow ends; the original interrupt handler cannot nest.
unsafe impl Sync for Receiver {}

impl Receiver {
    fn with_timer<R>(
        &self,
        operation: impl FnOnce(&mut CountdownTimer<'static, 'static>) -> R,
    ) -> R {
        let mut timer = self.timer.borrow_mut();
        operation(timer.as_mut().expect("original calibrated countdown"))
    }
}

fn interrupt(receiver: &Receiver, mut irq: Irq<'_>) {
    assert_eq!(irq.vector(), VECTOR);
    receiver.with_timer(|timer| unsafe { timer.accept_interrupt(&mut irq) }.unwrap());
    receiver.captured.fetch_add(1, Ordering::Release);
}

struct Initial {
    major_stack: Range<usize>,
    owner: RefCell<Option<LocalApic<'static>>>,
    complete: fn() -> !,
}

struct Parked {
    unit: UnitId,
    admission: AdmissionId,
    leave: Occupancy,
}

pub fn check(mut boot: Boot<'_>, complete: fn() -> !) -> ! {
    uefi_fixture::check_handoff(&boot, None);
    let description = boot.acpi().unwrap().hpet().unwrap().expect("original HPET");
    let range = description.registers();
    let window =
        unsafe { DeviceWindow::admit(range, InternalAddress::new(range.start().get())) }.unwrap();
    let hpet = unsafe { Hpet::prepare(description, window) }
        .unwrap_or_else(|(_, reason)| panic!("HPET preparation: {reason:?}"))
        .start()
        .unwrap_or_else(|(_, reason)| panic!("HPET start: {reason:?}"));
    let hpet = unsafe { (&mut *(&raw mut HPET)).write(hpet) };
    unsafe { (&mut *(&raw mut SIGN)).write(Sign::new(hpet.clock())) };

    let mut window = q35::local_window(&boot);
    unsafe { hardware::prepare(&mut window) };
    let mut owner = q35::local_controller(&boot, window);
    unsafe { owner.retire_inherited_timer() }.unwrap();
    let vector = owner.reserve_vector(VECTOR).unwrap();
    owner.reserve_timer(vector).unwrap();
    let initial = Initial {
        major_stack: boot.stack_range(),
        owner: RefCell::new(Some(owner)),
        complete,
    };
    let execution = Execution::new(
        boot.take_major().expect("original major"),
        boot.terminal(),
        unsafe { &mut *(&raw mut STACKS) },
        unsafe { &mut *(&raw mut RECORDS) },
        unsafe { &mut *(&raw mut PARTICIPANTS) },
        unsafe { &mut *(&raw mut DISPATCHERS) },
    )
    .map_err(|failure| failure.reason)
    .expect("original execution materials");
    let execution = unsafe { (&mut *(&raw mut EXECUTION)).write(execution) };
    let entry = unsafe { (&mut *(&raw mut RUNTIME)).write(RuntimeEntry::new(schedule, initial)) };
    assert_eq!(execution.attach(), Err(Error::Unsupported));
    unsafe { execution.run(entry) }.expect("resident dispatcher")
}

fn hpet() -> &'static Hpet {
    unsafe { (&*(&raw const HPET)).assume_init_ref() }
}

fn sign() -> &'static Sign<'static, 2> {
    unsafe { (&*(&raw const SIGN)).assume_init_ref() }
}

fn schedule(initial: &'static Initial) -> ! {
    let stack: usize;
    unsafe { asm!("mov {}, rsp", out(reg) stack, options(nomem, nostack)) };
    assert!(!initial.major_stack.contains(&stack));
    let execution = unsafe { (&*(&raw const EXECUTION)).assume_init_ref() };
    let owner = initial
        .owner
        .borrow_mut()
        .take()
        .expect("original APIC owner");
    unsafe { TRAP.install() }.unwrap();
    let timer = unsafe { CountdownTimer::calibrate(owner, hpet().clock()) }
        .unwrap_or_else(|(_, reason)| panic!("APIC calibration: {reason:?}"));
    let receiver = RECEPTION.owner();
    assert!(receiver.timer.replace(Some(timer)).is_none());
    execution.attach().expect("original Room participation");

    let late = park(execution, wait_late);
    execution.vacate(late.leave).unwrap();
    assert_eq!(service(execution), DeadlineProgress::Armed);
    let original = receiver.with_timer(|timer| timer.countdown().unwrap());

    let early = park(execution, wait_early);
    execution.vacate(early.leave).unwrap();
    assert_eq!(service(execution), DeadlineProgress::Armed);
    let advanced = receiver.with_timer(|timer| timer.countdown().unwrap());
    assert_eq!(advanced.divider, original.divider);
    assert!(advanced.ticks < original.ticks);
    assert_eq!(execution.enter(), Ok(None));
    assert_eq!(receiver.captured.load(Ordering::Acquire), 0);

    // A reminder may be early: retain the same Sign deadline and rearm
    // until the reference itself actually reaches it. No IRQ alone may
    // return either continuation.
    let mut received = 0;
    for (parked, value) in [(early, EARLY_END), (late, LATE_END)] {
        let origin = hpet().clock().sample().unwrap();
        let returned = loop {
            receive_next(received);
            received = receiver.captured.load(Ordering::Acquire);
            let progress = loop {
                let progress = service(execution);
                if progress != DeadlineProgress::Recheck {
                    break progress;
                }
                assert!(
                    hpet()
                        .clock()
                        .sample()
                        .unwrap()
                        .elapsed_since(origin)
                        .unwrap()
                        < Duration::from_secs(4)
                );
            };
            if let Some(occupancy) = execution.enter().unwrap() {
                break occupancy;
            }
            assert!(matches!(
                progress,
                DeadlineProgress::Armed | DeadlineProgress::AwaitInterrupt
            ));
            assert!(
                hpet()
                    .clock()
                    .sample()
                    .unwrap()
                    .elapsed_since(origin)
                    .unwrap()
                    < Duration::from_secs(4)
            );
        };
        finish(execution, parked, returned, value);
        // The first completion may have left the other wait armed.
        let progress = service(execution);
        if value == EARLY_END {
            assert!(matches!(
                progress,
                DeadlineProgress::Armed | DeadlineProgress::AwaitInterrupt
            ));
        }
    }

    // A later physical wake may still be owed if the reference won the
    // last ordinary recheck. Receive it before concluding the scenario.
    for _ in 0..8 {
        match service(execution) {
            DeadlineProgress::Idle => {
                assert!(received >= 2);
                assert_eq!(execution.enter(), Ok(None));
                assert_eq!(execution.pending_leave(), Ok(None));
                write(
                    "PASS x86-sign-deadline hpet=1 calibrated=1 advanced=1 native-irq=1 resumed=2 leaves=4\n",
                );
                (initial.complete)()
            }
            DeadlineProgress::Armed | DeadlineProgress::AwaitInterrupt => {
                receive_next(receiver.captured.load(Ordering::Acquire));
            }
            DeadlineProgress::Recheck => {}
        }
    }
    panic!("original reminder remained unsettled");
}

fn service(execution: &Execution) -> DeadlineProgress {
    RECEPTION.owner().with_timer(|timer| {
        unsafe { execution.service_deadlines(sign(), timer, NonZeroUsize::MIN) }.unwrap()
    })
}

fn receive_next(previous: usize) {
    let clock = hpet().clock();
    let start = clock.sample().unwrap();
    while RECEPTION.owner().captured.load(Ordering::Acquire) == previous {
        assert!(
            clock.sample().unwrap().elapsed_since(start).unwrap() < Duration::from_secs(4),
            "no actual local APIC receipt"
        );
        // All ordinary borrows have ended before permitting the exact IRQ.
        unsafe { asm!("sti", "nop", "cli", options(nostack)) };
    }
}

fn park(execution: &'static Execution, entry: fn(&mut Current) -> u64) -> Parked {
    let unit = unsafe { execution.create(entry) }.unwrap();
    let admission = execution.submit(unit).unwrap();
    let leave = execution.enter().unwrap().expect("original waiting Unit");
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

fn finish(execution: &Execution, parked: Parked, returned: Occupancy, value: u64) {
    assert_eq!(returned.unit(), parked.unit);
    assert_eq!(returned.admission(), parked.admission);
    assert_eq!(returned.processor(), parked.leave.processor());
    assert_ne!(returned, parked.leave);
    assert_eq!(execution.vacate(parked.leave), Err(Error::Gone));
    assert_eq!(execution.pending_leave(), Ok(Some(returned)));
    execution.vacate(returned).unwrap();
    execution.finish_end(parked.unit).unwrap();
    assert_eq!(execution.reap(parked.unit), Ok(End::Returned { value }));
}

fn wait_early(current: &mut Current) -> u64 {
    wait(current, EARLY, EARLY_END)
}

fn wait_late(current: &mut Current) -> u64 {
    wait(current, LATE, LATE_END)
}

fn wait(current: &mut Current, delay: Duration, value: u64) -> u64 {
    let clock = hpet().clock();
    assert_eq!(current.processor(), clock.processor());
    let initial = clock.sample().unwrap();
    let original = black_box(value);
    let address = core::ptr::from_ref(black_box(&original)).addr();
    unsafe { current.wait_until(sign(), initial.after(delay).unwrap()) }.unwrap();
    assert!(clock.sample().unwrap().elapsed_since(initial).unwrap() >= delay);
    assert_eq!(core::ptr::from_ref(black_box(&original)).addr(), address);
    black_box(original)
}
