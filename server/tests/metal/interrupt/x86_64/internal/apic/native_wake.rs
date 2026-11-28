//! A real fixed-vector IPI through the final idle check and native halt.

use core::{
    arch::asm,
    cell::UnsafeCell,
    hint::{black_box, spin_loop},
    mem::MaybeUninit,
    ptr,
    sync::atomic::{
        AtomicBool, AtomicUsize,
        Ordering::{Acquire, Release},
    },
};

use kernel::{
    basis::{
        execution::{Current, Error, Execution, KernelBinding, RuntimeEntry},
        room::{Occupancy, Participants},
        unit::{End, Stacks, UnitRecord},
    },
    entry::{boot::Boot, hook::Binding},
    metal::{
        interrupt::{
            treaty::{Completion, HardwareClaim, Issuance, NotifyProcessor},
            x86_64::internal::apic::{
                Failure, IpiRoute, IpiTarget, LocalApic, Probe, Reception, Wake, WakeReceiver,
            },
        },
        memory::access::treaty::device::DeviceWindow,
        platform::{boot::treaty::write, description::treaty::acpi::Madt},
        processor::treaty::{
            Dispatchers, Irq, LocalStorage, Minor, ProcessorId, RuntimeTrap, Stack, Startup,
        },
        sync::treaty::{Supply, TurnAcquireError, TurnLock},
    },
};
use limen_treaty::model::{ByteLength, InternalAddress};
use uefi_fixture as fixture;

#[path = "../../prepare.rs"]
mod hardware;

const VECTOR: u8 = 0xf3;
const ROOM_ROUND: usize = 4;
const RESULT: u64 = 42;
static MINOR_STACK: Stack<{ 32 * 1024 }> = Stack::new();
static MINOR_LOCAL: LocalStorage<5> = LocalStorage::new(Supply::new());
static MINOR: Minor = Minor::new(&MINOR_STACK, &MINOR_LOCAL);
static TURN: TurnLock<()> = TurnLock::new(());
static WORK: AtomicUsize = AtomicUsize::new(0);
static WINDOW: AtomicUsize = AtomicUsize::new(0);
static RECHECKED: AtomicUsize = AtomicUsize::new(0);
static RECEIVED: AtomicUsize = AtomicUsize::new(0);
static mut STACKS: Stacks = Stacks::new();
static mut RECORDS: [UnitRecord<KernelBinding>; 1] = [const { UnitRecord::EMPTY }; 1];
static mut PARTICIPANTS: Participants<1> = Participants::new();
static mut DISPATCHERS: Dispatchers<Occupancy, 1> = Dispatchers::new();
static mut EXECUTION: MaybeUninit<Execution> = MaybeUninit::uninit();
static RUNTIME: RuntimeEntry<()> = RuntimeEntry::new(schedule, ());

struct Receiver {
    controller: UnsafeCell<Option<LocalApic<'static>>>,
    endpoint: UnsafeCell<MaybeUninit<WakeReceiver>>,
    published: AtomicBool,
}

// Only the admitted destination accesses the register owner. Its IRQ borrow
// starts after the ordinary borrow ends. The endpoint is published once.
unsafe impl Sync for Receiver {}

static HOOK: Binding<Receiver> = Binding::new(
    Receiver {
        controller: UnsafeCell::new(None),
        endpoint: UnsafeCell::new(MaybeUninit::uninit()),
        published: AtomicBool::new(false),
    },
    interrupt,
);
static TRAP: RuntimeTrap = RuntimeTrap::new(VECTOR, HOOK.entry());

struct Launch<'a> {
    madt: Madt<'a>,
    source: ProcessorId,
    destination: IpiTarget,
}

fn interrupt(receiver: &Receiver, mut irq: Irq<'_>) {
    assert_eq!(TURN.acquire(|_| ()), Err(TurnAcquireError::Context));
    assert!(receiver.published.load(Acquire));
    let endpoint = unsafe { (&*receiver.endpoint.get()).assume_init_ref() };
    assert_eq!(irq.vector(), endpoint.vector());
    let controller = unsafe { (&mut *receiver.controller.get()).as_mut().unwrap() };
    let mut claim = unsafe { controller.receive_irq(&mut irq) }.unwrap();
    assert_eq!(claim.source_number(), u32::from(VECTOR));
    assert_eq!(unsafe { claim.finish() }, Ok(Completion::Settled));
    RECEIVED.fetch_add(1, Release);
}

fn await_phase(phase: &AtomicUsize, expected: usize) {
    let start = fixture::q35::tsc();
    let interval = fixture::q35::tsc_frequency();
    while phase.load(Acquire) != expected {
        assert!(fixture::q35::tsc().wrapping_sub(start) < interval);
        spin_loop();
    }
}

fn masked() -> bool {
    let flags: usize;
    unsafe { asm!("pushfq", "pop {}", out(reg) flags, options(preserves_flags)) };
    flags & (1 << 9) == 0
}

fn execution() -> &'static Execution {
    // The major initializes this before releasing the minor.
    unsafe { (&*(&raw const EXECUTION)).assume_init_ref() }
}

fn unit(_: &mut Current) -> u64 {
    RESULT
}

fn minor(processor: usize, argument: usize) -> ! {
    fixture::check_minor(&MINOR, processor);
    // The major retains the immutable launch arguments until this guest exits.
    let launch = unsafe { &*ptr::with_exposed_provenance::<Launch<'_>>(argument) };
    let address = launch.madt.local_controller_address().unwrap();
    let physical = address.range(ByteLength::new(4096)).unwrap();
    let mut window =
        unsafe { DeviceWindow::admit(physical, InternalAddress::new(address.get())) }.unwrap();
    unsafe { hardware::prepare(&mut window) };
    let mut controller =
        unsafe { LocalApic::admit(processor, launch.madt, window, Probe::native()) }
            .ok()
            .unwrap();
    assert!(matches!(
        unsafe { WakeReceiver::admit(&mut controller, launch.destination, launch.source) },
        Err(Failure::Scope)
    ));
    unsafe { TRAP.install() }.unwrap();
    let endpoint =
        unsafe { WakeReceiver::admit(&mut controller, launch.destination, launch.source) }.unwrap();
    assert!(endpoint.idle(&mut controller).is_ok());
    let receiver = HOOK.owner();
    unsafe {
        receiver.controller.get().write(Some(controller));
        (*receiver.endpoint.get()).write(endpoint);
    }
    receiver.published.store(true, Release);
    let endpoint = unsafe { (&*receiver.endpoint.get()).assume_init_ref() };
    let controller = unsafe { (&mut *receiver.controller.get()).as_mut().unwrap() };
    assert!(matches!(
        endpoint.bind_sender(controller),
        Err(Failure::Scope)
    ));

    assert!(matches!(execution().attach(), Err(Error::Unsupported)));
    unsafe { execution().run(&RUNTIME) }.expect("resident minor dispatcher")
}

fn schedule(_: &'static ()) -> ! {
    let mark = black_box(0u8);
    assert!(
        !MINOR
            .stack_range()
            .contains(&ptr::from_ref(black_box(&mark)).addr())
    );
    let execution = execution();
    execution.attach().expect("attach from resident dispatcher");
    let receiver = HOOK.owner();
    let endpoint = unsafe { (&*receiver.endpoint.get()).assume_init_ref() };
    let idle = || {
        let controller = unsafe { (&mut *receiver.controller.get()).as_mut().unwrap() };
        endpoint.idle(controller)
    };
    TURN.acquire(|_| assert!(matches!(idle(), Err(Failure::Busy))))
        .unwrap();

    // Publish before the final check. A ready observation cannot need a hint.
    await_phase(&WORK, 1);
    assert_eq!(
        unsafe { idle().unwrap().wait(|| WORK.load(Acquire) == 1) },
        Ok(Wake::Ready)
    );
    assert_eq!(RECEIVED.load(Acquire), 0);
    RECHECKED.store(1, Release);

    for round in 2..=3 {
        let mut first = true;
        let result = unsafe {
            idle().unwrap().wait(|| {
                if first {
                    first = false;
                    assert_eq!(WORK.load(Acquire), 1);
                    WINDOW.store(round, Release);
                    false
                } else {
                    WORK.load(Acquire) == round
                }
            })
        };
        assert_eq!(
            result,
            Ok(if round == 2 {
                Wake::Recheck
            } else {
                Wake::Ready
            })
        );
        assert!(masked());
        assert_eq!(RECEIVED.load(Acquire), round - 1);
        let controller = unsafe { (&*receiver.controller.get()).as_ref().unwrap() };
        assert_eq!(controller.reception(), Reception::Idle);
        RECHECKED.store(round, Release);
    }

    let observation = execution.observe_idle().unwrap().expect("Room empty");
    let mut first = true;
    let observation_changed = unsafe {
        idle().unwrap().wait(|| {
            let changed = observation.changed();
            if first {
                first = false;
                assert!(!changed);
                WINDOW.store(ROOM_ROUND, Release);
            }
            changed
        })
    };
    assert_eq!(observation_changed, Ok(Wake::Ready));
    assert!(masked());
    assert_eq!(RECEIVED.load(Acquire), 3);
    let controller = unsafe { (&*receiver.controller.get()).as_ref().unwrap() };
    assert_eq!(controller.reception(), Reception::Idle);
    let occupied = execution.enter().unwrap().expect("original Ready Unit");
    assert_eq!(occupied.processor(), fixture::q35::processor());
    assert_eq!(execution.pending_leave(), Ok(Some(occupied)));
    execution.vacate(occupied).expect("matching native leave");
    assert_eq!(execution.pending_leave(), Ok(None));
    execution.finish_end(occupied.unit()).unwrap();
    assert_eq!(
        execution.reap(occupied.unit()),
        Ok(End::Returned { value: RESULT })
    );
    assert!(execution.observe_idle().unwrap().is_some());
    RECHECKED.store(ROOM_ROUND, Release);
    loop {
        spin_loop()
    }
}

fn issue(sender: &mut IpiRoute<'_, '_>) {
    let Issuance::Pending(original) = sender.notify().unwrap() else {
        panic!("fixed ICR issue is retained before confirmation")
    };
    let start = fixture::q35::tsc();
    let interval = fixture::q35::tsc_frequency();
    loop {
        match sender.confirm(original).unwrap() {
            Issuance::Confirmed(replayed) => {
                assert_eq!(replayed, original);
                break;
            }
            Issuance::Pending(_) => assert!(fixture::q35::tsc().wrapping_sub(start) < interval),
            Issuance::Unconfirmed(_, _) => panic!("original effect is unknown"),
        }
        spin_loop();
    }
}

pub fn check(mut boot: Boot<'_>) {
    let major = boot.take_major().expect("admitted major");
    let runtime = Execution::new(
        major,
        boot.terminal(),
        unsafe { &mut *(&raw mut STACKS) },
        unsafe { &mut *(&raw mut RECORDS) },
        unsafe { &mut *(&raw mut PARTICIPANTS) },
        unsafe { &mut *(&raw mut DISPATCHERS) },
    )
    .map_err(|failure| failure.reason)
    .expect("admit cooperative profile");
    unsafe { (&mut *(&raw mut EXECUTION)).write(runtime) };
    let execution = execution();
    let unit = unsafe { execution.create_migratable(unit) }.expect("native wake Unit");

    let view = fixture::check_handoff(&boot, Some(&MINOR));
    let mut startup = fixture::inspect(&boot, &view);
    let madt = boot.acpi().unwrap().madt().unwrap();
    let destination = fixture::target();
    let launch = Launch {
        madt,
        source: ProcessorId::new(boot.processor()),
        destination: IpiTarget::describe(madt, destination, VECTOR).unwrap(),
    };
    unsafe {
        MINOR.start(
            destination,
            minor,
            ptr::from_ref(&launch).expose_provenance(),
            &mut startup,
        )
    }
    .unwrap();
    let receiver = HOOK.owner();
    let start = fixture::q35::tsc();
    let interval = fixture::q35::tsc_frequency();
    while !receiver.published.load(Acquire) {
        assert!(fixture::q35::tsc().wrapping_sub(start) < interval);
        spin_loop();
    }
    assert!(MINOR.ready());
    let endpoint = unsafe { (&*receiver.endpoint.get()).assume_init_ref() };
    let Startup::Xapic { mut controller, .. } = startup;
    let mut sender = endpoint.bind_sender(&mut controller).unwrap();
    WORK.store(1, Release);
    await_phase(&RECHECKED, 1);
    await_phase(&WINDOW, 2);
    issue(&mut sender);
    await_phase(&RECHECKED, 2);
    await_phase(&WINDOW, 3);
    WORK.store(3, Release);
    issue(&mut sender);
    await_phase(&RECHECKED, 3);
    write("PASS x86 native idle: final-check=2 receive=2 eoi=2 if-masked=2 ready=2 recheck=1\n");
    await_phase(&WINDOW, ROOM_ROUND);
    execution.submit(unit).expect("original Room Ready");
    issue(&mut sender);
    await_phase(&RECHECKED, ROOM_ROUND);
    assert_eq!(execution.lifecycle(unit), Err(Error::Gone));
    write("PASS x86 room-idle resident=1 ready=1 ipi=1 eoi=1 dispatched=1 leave=1 reaped=1\n");
}
