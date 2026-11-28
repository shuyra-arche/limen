//! SBI admission and native wake for the shared cross-processor migration.

use core::{
    arch::asm,
    cell::UnsafeCell,
    hint::spin_loop,
    sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed},
};
use kernel::{
    basis::room::IdleObservation,
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

pub(super) fn await_ready(processor: usize, idle: IdleObservation, announce: impl FnOnce()) {
    unsafe { TRAP.install() }.unwrap();
    let owner = RECEPTION.owner();
    let mut receiver = unsafe { Receiver::admit(processor) }.unwrap();
    receiver.open().unwrap();
    assert_eq!(receiver.pending(), Ok(false));
    unsafe { owner.receiver.get().write(Some(receiver)) };

    let start = count();
    let limit = 5 * u64::from(HERTZ.load(Relaxed));
    let mut announce = Some(announce);
    {
        let receiver = unsafe { (&*owner.receiver.get()).as_ref().unwrap() };
        loop {
            let result = unsafe {
                receiver.wait(|| {
                    let changed = idle.changed();
                    if let Some(announce) = announce.take() {
                        assert!(!changed);
                        announce();
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

pub(super) fn notify(boot: &Boot<'_>, processor: usize) {
    let cpus = boot.riscv_cpus();
    let services = boot.sbi_services();
    let target = Target::describe(&cpus.cpu(processor as u64).unwrap().unwrap()).unwrap();
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
    let mut route = unsafe { sender.bind_ipi(&target) }.unwrap();
    assert!(matches!(route.notify(), Ok(Issuance::Confirmed(_))));
    assert_eq!(route.pending(), Ok(None));
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
        &mut Startup::Sbi(service),
        observe,
        |_| {},
        complete,
    )
}
