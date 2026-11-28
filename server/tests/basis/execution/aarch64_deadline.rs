//! Pure Sign deadlines through the original AArch64 virtual timer and GIC PPI.

use core::{
    arch::asm, hint::black_box, mem::MaybeUninit, num::NonZeroUsize, ops::Range, time::Duration,
};
use kernel::{
    basis::{
        execution::{Current, DeadlineProgress, Error, Execution, KernelBinding, RuntimeEntry},
        room::{AdmissionId, Occupancy, Participants},
        sign::Sign,
        unit::{End, Lifecycle, Stacks, UnitId, UnitRecord},
    },
    entry::boot::Boot,
    metal::{
        chrono::aarch64::{counter::VirtualCounter, description::GenericCounter},
        interrupt::aarch64::{
            gic::{
                GicV3,
                cpu_interface::CpuInterface,
                reception::State as Reception,
                redistributor::{Redistributor, Slot, input::description::PrivateInterrupts},
            },
            timer::{GenericTimer, State, VirtualTimer},
        },
        memory::access::treaty::device::DeviceWindow,
        platform::{boot::treaty::write, description::treaty::devicetree::DeviceTree},
        processor::treaty::Dispatchers,
    },
};
use limen_treaty::model::{InternalAddress, PhysicalRange};

#[path = "../../metal/interrupt/aarch64/gic/native/timer.rs"]
mod reception;

static mut STACKS: Stacks = Stacks::new();
static mut RECORDS: [UnitRecord<KernelBinding>; 2] = [const { UnitRecord::EMPTY }; 2];
static mut PARTICIPANTS: Participants<1> = Participants::new();
static mut DISPATCHERS: Dispatchers<Occupancy, 1> = Dispatchers::new();
static mut EXECUTION: MaybeUninit<Execution> = MaybeUninit::uninit();
static mut RUNTIME: MaybeUninit<RuntimeEntry<Initial>> = MaybeUninit::uninit();
static mut TREE: [u8; 2 * 1024 * 1024] = [0; 2 * 1024 * 1024];
static mut COUNTER: MaybeUninit<VirtualCounter<'static>> = MaybeUninit::uninit();
static mut SIGN: MaybeUninit<Sign<'static, 2>> = MaybeUninit::uninit();

const EARLY: Duration = Duration::from_millis(200);
const LATE: Duration = Duration::from_secs(2);
const EARLY_END: u64 = 1;
const LATE_END: u64 = 2;

struct Initial {
    processor: usize,
    major_stack: Range<usize>,
    tree: DeviceTree<'static>,
    complete: fn() -> !,
}

struct Parked {
    unit: UnitId,
    admission: AdmissionId,
    leave: Occupancy,
}

pub fn check(mut boot: Boot<'_>, complete: fn() -> !) -> ! {
    let original = boot
        .device_tree_description()
        .expect("original checked DTB");
    let bytes = original.bytes();
    let storage = unsafe { &mut *(&raw mut TREE) };
    let retained = storage.get_mut(..bytes.len()).expect("QEMU DTB capacity");
    retained.copy_from_slice(bytes);
    let initial = Initial {
        processor: boot.processor(),
        major_stack: boot.stack_range(),
        tree: DeviceTree::new(retained).expect("retained checked DTB"),
        complete,
    };
    let execution = Execution::new(
        boot.take_major().expect("original major relation"),
        boot.terminal(),
        unsafe { &mut *(&raw mut STACKS) },
        unsafe { &mut *(&raw mut RECORDS) },
        unsafe { &mut *(&raw mut PARTICIPANTS) },
        unsafe { &mut *(&raw mut DISPATCHERS) },
    )
    .map_err(|failure| failure.reason)
    .expect("original execution material");
    let execution = unsafe { (&mut *(&raw mut EXECUTION)).write(execution) };
    let entry = unsafe { (&mut *(&raw mut RUNTIME)).write(RuntimeEntry::new(schedule, initial)) };
    assert_eq!(execution.attach(), Err(Error::Unsupported));
    unsafe { execution.run(entry) }.expect("resident dispatcher")
}

fn window(range: PhysicalRange) -> DeviceWindow<'static> {
    unsafe { DeviceWindow::admit(range, InternalAddress::new(range.start().get())).unwrap() }
}

fn schedule(initial: &'static Initial) -> ! {
    let stack: usize;
    unsafe { asm!("mov {}, sp", out(reg) stack, options(nostack, nomem)) };
    assert!(!initial.major_stack.contains(&stack));

    let tree = &initial.tree;
    let controller = GicV3::read(tree).unwrap().expect("physical GIC");
    let description = GenericCounter::read(tree)
        .unwrap()
        .expect("virtual counter");
    let counter = unsafe { description.bind(initial.processor) }.unwrap();
    let counter = unsafe { (&mut *(&raw mut COUNTER)).write(counter) };
    let bindings = PrivateInterrupts::read(tree, counter.source(), controller).unwrap();
    let description = GenericTimer::describe(tree, &bindings).unwrap();
    let mut slots = [const { Slot::EMPTY }; 8];
    let mut frame =
        unsafe { Redistributor::admit(controller, window(controller.redistributor()), &mut slots) }
            .unwrap()
            .find(|frame| frame.affinity() == initial.processor)
            .expect("original PE redistributor");
    for _ in 0..4096 {
        if frame.wake().is_ok() {
            break;
        }
    }
    assert!(frame.awake());
    let mut distributor = window(controller.distributor());
    let control = unsafe { distributor.read_u32(0) }.unwrap();
    // QEMU virt uses the single security-state distributor with non-secure
    // affinity routing. Enable its Group 1 forwarding and this PE's receiver.
    assert_ne!(control & (1 << 6), 0);
    unsafe {
        distributor.write_u32(0, control | (1 << 1)).unwrap();
        asm!(
            "dsb sy", "isb",
            "msr ICC_PMR_EL1, {}", "msr ICC_IGRPEN1_EL1, {}", "isb",
            in(reg) 0x80u64, in(reg) 1u64, options(nostack),
        );
    }
    let mut ppi = unsafe { frame.reserve_ppi(&bindings, description.virtual_interrupt(), 0x10) }
        .expect("original virtual timer PPI");
    let mut receiver = unsafe { CpuInterface::admit(frame) }
        .unwrap_or_else(|(_, reason)| panic!("timer GIC interface: {reason:?}"));
    let execution = unsafe { (&*(&raw const EXECUTION)).assume_init_ref() };
    let sign = unsafe { (&mut *(&raw mut SIGN)).write(Sign::new(counter.clock())) };
    reception::install();
    execution
        .attach()
        .expect("Room participation on resident stack");

    let late = park(execution, wait_late);
    execution.vacate(late.leave).unwrap();
    receiver.prepare_ppi(&mut ppi).unwrap();
    let mut timer = unsafe { VirtualTimer::admit(description, counter, &mut ppi) }.unwrap();
    assert_eq!(
        service(execution, sign, &mut timer),
        DeadlineProgress::Armed
    );
    let later = comparison();
    assert_eq!(timer.state(), State::Armed(later));

    let early = park(execution, wait_early);
    assert_eq!(
        service(execution, sign, &mut timer),
        DeadlineProgress::Armed
    );
    let earlier = comparison();
    assert!(earlier < later);
    assert_eq!(timer.state(), State::Armed(earlier));
    wait_for_ppi(&timer);
    assert_eq!(
        service(execution, sign, &mut timer),
        DeadlineProgress::AwaitInterrupt
    );
    assert_eq!(timer.pending(), Ok(true));
    assert_eq!(execution.pending_leave(), Ok(Some(early.leave)));
    reception::receive(&mut receiver, &mut timer, later);
    assert_eq!(timer.state(), State::Idle);
    assert!(timer.stopped_ppi().is_none());
    assert_eq!(receiver.state(), Reception::Empty);
    assert_eq!(
        service(execution, sign, &mut timer),
        DeadlineProgress::Recheck
    );
    assert_eq!(execution.enter(), Err(Error::Busy));
    execution
        .vacate(early.leave)
        .expect("exact early machine leave");

    // The completed PPI has returned to the original receiver. Reopen that
    // exact route and admit the disabled comparator before programming again.
    drop(timer);
    receiver.prepare_ppi(&mut ppi).unwrap();
    let mut timer = unsafe { VirtualTimer::admit(description, counter, &mut ppi) }.unwrap();
    assert_eq!(
        service(execution, sign, &mut timer),
        DeadlineProgress::Armed
    );
    assert_eq!(comparison(), later);
    finish(execution, early, EARLY_END);
    assert_eq!(execution.enter(), Ok(None));

    wait_for_ppi(&timer);
    assert_eq!(
        service(execution, sign, &mut timer),
        DeadlineProgress::AwaitInterrupt
    );
    assert_eq!(timer.pending(), Ok(true));
    reception::receive(
        &mut receiver,
        &mut timer,
        later + counter.clock().frequency().get(),
    );
    assert_eq!(service(execution, sign, &mut timer), DeadlineProgress::Idle);
    assert_eq!(timer.state(), State::Idle);
    assert_eq!(receiver.state(), Reception::Empty);
    finish(execution, late, LATE_END);
    assert_eq!(execution.enter(), Ok(None));

    let mut frame = receiver
        .release()
        .unwrap_or_else(|_| panic!("timer receiver remains live"));
    for _ in 0..4096 {
        match unsafe { frame.retire_ppi(ppi) } {
            Ok(()) => {
                unsafe { distributor.write_u32(0, control).unwrap() };
                write(
                    "PASS arm-sign-deadline armed=2 advanced=1 ppi=2 eoi=2 resumed=2 leaves=4 retired=1\n",
                );
                (initial.complete)()
            }
            Err((returned, _)) => ppi = returned,
        }
    }
    panic!("original timer PPI still in use");
}

fn wait_for_ppi(timer: &VirtualTimer<'_, '_, '_>) {
    // The source remains enabled and physical reception masked across WFI.
    // A pending level wakes WFI without consuming the original IAR claim.
    assert_eq!(timer.pending(), Ok(false));
    loop {
        unsafe { asm!("dsb sy", "wfi", "isb", options(nostack)) };
        if timer.pending().unwrap() {
            return;
        }
    }
}

fn comparison() -> u64 {
    let count;
    unsafe { asm!("mrs {}, CNTV_CVAL_EL0", out(reg) count, options(nostack, nomem)) };
    count
}

fn service(
    execution: &Execution,
    sign: &Sign<'_, 2>,
    timer: &mut VirtualTimer<'_, '_, '_>,
) -> DeadlineProgress {
    unsafe { execution.service_deadlines(sign, timer, NonZeroUsize::MIN) }.unwrap()
}

fn park(execution: &'static Execution, entry: fn(&mut Current) -> u64) -> Parked {
    let unit = unsafe { execution.create(entry) }.unwrap();
    let admission = execution.submit(unit).unwrap();
    let leave = execution.enter().unwrap().expect("parked Unit");
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
    let counter = unsafe { (&*(&raw const COUNTER)).assume_init_ref() };
    let sign = unsafe { (&*(&raw const SIGN)).assume_init_ref() };
    let clock = counter.clock();
    assert_eq!(current.processor(), clock.processor());
    let start = clock.sample().unwrap();
    let original = black_box(value);
    let address = core::ptr::from_ref(black_box(&original)).addr();
    unsafe { current.wait_until(sign, start.after(delay).unwrap()) }.unwrap();
    assert!(clock.sample().unwrap().elapsed_since(start).unwrap() >= delay);
    assert_eq!(core::ptr::from_ref(black_box(&original)).addr(), address);
    black_box(original)
}
