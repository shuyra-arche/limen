//! SBI admission and native wake for the shared cross-processor migration.

use core::{
    arch::asm,
    cell::UnsafeCell,
    hint::spin_loop,
    sync::atomic::{
        AtomicBool, AtomicU32,
        Ordering::{Acquire, Relaxed, Release},
    },
};
use kernel::{
    basis::{
        execution::{Current, Error, Execution},
        room::IdleObservation,
        unit::{End, Lifecycle},
    },
    entry::{boot::Boot, hook::Binding},
    metal::{
        chrono::riscv64::description::RiscvTimebase,
        interrupt::{
            riscv64::internal::ipi::{Receiver, SendState, Sender, Target, Wake},
            treaty::{Issuance, NotifyProcessor},
        },
        processor::treaty::{Minor, RuntimeTrap, SoftwareInterrupt, Startup},
    },
};

struct Reception {
    receiver: UnsafeCell<Option<Receiver>>,
    received: AtomicBool,
}

// This receiver belongs to the minor. Every ordinary borrow ends before
// SIE can be enabled and the installed software IRQ cannot nest.
unsafe impl Sync for Reception {}

static RECEPTION: Binding<Reception> = Binding::new(
    Reception {
        receiver: UnsafeCell::new(None),
        received: AtomicBool::new(false),
    },
    Some(interrupt),
    None,
);
static TRAP: RuntimeTrap = RuntimeTrap::new(RECEPTION.entry());
static HERTZ: AtomicU32 = AtomicU32::new(0);
static WAITING: AtomicBool = AtomicBool::new(false);
static SETTLED: AtomicBool = AtomicBool::new(false);
const RESULT: u64 = 37;

fn interrupt(reception: &Reception, interrupt: SoftwareInterrupt<'_>) {
    let receiver = unsafe { (&mut *reception.receiver.get()).as_mut().unwrap() };
    receiver.acknowledge_interrupt(&interrupt).unwrap();
    assert!(!reception.received.swap(true, Relaxed));
}

fn count() -> u64 {
    let count;
    unsafe { asm!("rdtime {}", out(reg) count, options(nostack, nomem)) };
    count
}

fn await_ready(processor: usize, idle: IdleObservation) {
    unsafe { TRAP.install() }.unwrap();
    let owner = RECEPTION.owner();
    let mut receiver = unsafe { Receiver::admit(processor) }.unwrap();
    receiver.open().unwrap();
    assert_eq!(receiver.pending(), Ok(false));
    unsafe { owner.receiver.get().write(Some(receiver)) };

    let start = count();
    let limit = 5 * u64::from(HERTZ.load(Relaxed));
    {
        let receiver = unsafe { (&*owner.receiver.get()).as_ref().unwrap() };
        loop {
            let result = unsafe {
                receiver.wait(|| {
                    let changed = idle.changed();
                    if !WAITING.load(Relaxed) {
                        assert!(!changed);
                        WAITING.store(true, Release);
                    }
                    changed
                })
            };
            if result.unwrap() != Wake::Recheck {
                break;
            }
            assert!(count().wrapping_sub(start) < limit);
        }
        while !receiver.pending().unwrap() {
            assert!(count().wrapping_sub(start) < limit);
            spin_loop();
        }
    }
    while !owner.received.load(Relaxed) {
        assert!(count().wrapping_sub(start) < limit);
        unsafe {
            asm!(
                "csrsi sstatus, 2",
                "nop",
                "csrci sstatus, 2",
                options(nostack)
            )
        };
    }
    assert!(idle.changed());
    let mut receiver = unsafe { (&mut *owner.receiver.get()).take().unwrap() };
    assert_eq!(receiver.pending(), Ok(false));
    unsafe { receiver.shut() }.unwrap();
}

fn notified(current: &mut Current) -> u64 {
    assert_eq!(current.processor(), super::PEER.load(Relaxed));
    RESULT
}

pub(super) fn dispatch_notified(execution: &'static Execution, processor: usize) {
    let idle = execution
        .observe_idle()
        .unwrap()
        .expect("compatible queue empty");
    await_ready(processor, idle);
    let occupancy = execution.enter().unwrap().expect("notified Ready Unit");
    assert_eq!(occupancy.processor(), processor);
    assert_eq!(execution.pending_leave(), Ok(Some(occupancy)));
    assert_eq!(execution.finish_end(occupancy.unit()), Err(Error::Busy));
    execution.vacate(occupancy).unwrap();
    execution.finish_end(occupancy.unit()).unwrap();
    assert_eq!(execution.pending_leave(), Ok(None));
    SETTLED.store(true, Release);
}

fn await_flag(flag: &AtomicBool, state: &str) {
    let start = count();
    let limit = 5 * u64::from(HERTZ.load(Relaxed));
    while !flag.load(Acquire) {
        assert!(count().wrapping_sub(start) < limit, "{state}");
        spin_loop();
    }
}

pub(super) fn submit_and_notify(boot: &Boot<'_>, execution: &Execution, processor: usize) {
    assert!(matches!(execution.observe_idle(), Err(Error::Unsupported)));
    let unit = unsafe { execution.create_migratable(notified) }.unwrap();
    let cpus = boot.riscv_cpus();
    let services = boot.sbi_services();
    let target = Target::describe(&cpus.cpu(processor as u64).unwrap().unwrap()).unwrap();
    // This scenario's only sender uses the original CPU description and
    // retains the issuance state until the peer has dispatched and left.
    let mut state = SendState::FRESH;
    let mut sender = unsafe {
        Sender::bind(
            cpus,
            boot.processor(),
            &mut state,
            services.hart_state_management().unwrap().unwrap(),
            services.ipi().unwrap().unwrap(),
        )
    }
    .unwrap();
    await_flag(&WAITING, "minor fixed idle observation");
    execution.submit(unit).unwrap();
    let mut route = unsafe { sender.bind_ipi(&target) }.unwrap();
    assert!(matches!(route.notify(), Ok(Issuance::Confirmed(_))));
    assert_eq!(route.pending(), Ok(None));
    await_flag(&SETTLED, "minor Room leave");
    assert_eq!(
        execution.lifecycle(unit),
        Ok(Lifecycle::Ended(End::Returned { value: RESULT }))
    );
    assert_eq!(execution.reap(unit), Ok(End::Returned { value: RESULT }));
    kernel::metal::platform::boot::treaty::write(
        "PASS room-idle resident_dispatcher=1 pre_attach_submit=1 sbi_ipi=1 ack=1 dispatched=1 leave=1\n",
    );
}

fn observe(minor: &Minor, _: usize) {
    let sp: usize;
    unsafe { core::arch::asm!("mv {}, sp", out(reg) sp, options(nomem, nostack)) };
    assert!(minor.ready() && minor.stack_range().contains(&sp));
}

pub fn check(boot: Boot<'_>, complete: fn() -> !) -> ! {
    let timebase = RiscvTimebase::describe(boot.riscv_cpus()).unwrap().unwrap();
    HERTZ.store(timebase.hertz(), Relaxed);
    let target = (0..4)
        .filter(|hart| *hart != boot.processor())
        .find(|hart| {
            boot.riscv_cpus()
                .cpu(*hart as u64)
                .unwrap()
                .is_some_and(|cpu| cpu.enabled())
        })
        .expect("another enabled Hart");
    let service = boot
        .sbi_services()
        .hart_state_management()
        .unwrap()
        .expect("SBI HSM");
    super::run(
        boot,
        target,
        Startup::Sbi(service),
        observe,
        |_| {},
        complete,
    )
}
