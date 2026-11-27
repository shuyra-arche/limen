use kernel::metal::{
    processor::treaty::{LocalStorage, Stack},
    sync::treaty::Supply,
};

use kernel::entry::hook::Binding;
use kernel::metal::platform::boot::treaty as boot_diagnostic;
use kernel::metal::processor::treaty::{
    InstallError, Minor, RuntimeTrap, SoftwareInterrupt, Startup,
};

use core::{
    arch::asm,
    cell::UnsafeCell,
    hint::spin_loop,
    sync::atomic::{
        AtomicUsize,
        Ordering::{Acquire, Relaxed, Release},
    },
};
use kernel::{
    entry::boot::Boot,
    metal::{
        chrono::riscv64::{description::RiscvTimebase, time::Counter},
        interrupt::riscv64::internal::ipi::{Error, Receiver, SendState, Sender, Target, Wake},
        sync::treaty::{TurnAcquireError, TurnLock},
    },
};

#[path = "../../ipi.rs"]
mod ipi;

static MINOR_STACK: Stack<{ 32 * 1024 }> = Stack::new();
static MINOR_LOCAL: LocalStorage<5> = LocalStorage::new(Supply::new());
static MINOR: Minor = Minor::new(&MINOR_STACK, &MINOR_LOCAL);
static READY: AtomicUsize = AtomicUsize::new(0);
static MAILBOX: ipi::Mailbox = ipi::Mailbox::new();
static FINAL_CHECK: AtomicUsize = AtomicUsize::new(0);
static CAPTURES: AtomicUsize = AtomicUsize::new(0);
static ORDINARY: TurnLock<usize> = TurnLock::new(0);
static TRAP: RuntimeTrap = RuntimeTrap::new(RECEPTION.entry());

struct ReceiverSlot(UnsafeCell<Option<Receiver>>);

// Only the minor processor uses this slot. Its ordinary code borrows the
// receiver only while native interrupts are masked.
unsafe impl Sync for ReceiverSlot {}

static RECEPTION: Binding<ReceiverSlot> =
    Binding::new(ReceiverSlot(UnsafeCell::new(None)), Some(interrupt), None);

fn interrupt(owner: &ReceiverSlot, capture: SoftwareInterrupt<'_>) {
    assert_eq!(ORDINARY.acquire(|_| ()), Err(TurnAcquireError::Context));
    let receiver = unsafe { (&mut *owner.0.get()).as_mut().unwrap() };
    receiver.acknowledge_interrupt(&capture).unwrap();
    CAPTURES.fetch_add(1, Relaxed);
}

fn receive(round: usize, until: u64) {
    let argument: usize;
    let temporary: usize;
    let stack_before: usize;
    let stack_after: usize;
    let status: usize;
    unsafe {
        asm!(
            "mv {before}, sp",
            "li a0, 0xa17",
            "li t0, 0x5c2",
            "csrsi sstatus, 2",
            "2: ld t1, 0({captures})",
            "beq t1, {round}, 3f",
            "rdtime t2",
            "bltu t2, {until}, 2b",
            "3: csrr {status}, sstatus",
            "csrci sstatus, 2",
            "mv {after}, sp",
            before = out(reg) stack_before,
            after = out(reg) stack_after,
            status = out(reg) status,
            captures = in(reg) &CAPTURES,
            round = in(reg) round,
            until = in(reg) until,
            out("a0") argument,
            out("t0") temporary,
            out("t1") _,
            out("t2") _,
            options(nostack),
        );
    }
    assert_eq!(stack_before, stack_after);
    assert_eq!(argument, 0xa17);
    assert_eq!(temporary, 0x5c2);
    assert_ne!(status & 2, 0);
    assert_eq!(CAPTURES.load(Relaxed), round);
}

fn now() -> u64 {
    let count;
    unsafe { asm!("rdtime {}", out(reg) count, options(nostack, nomem)) };
    count
}

fn wait(state: &AtomicUsize, expected: usize, hertz: u64) {
    let until = now() + 5 * hertz;
    while state.load(Acquire) != expected {
        assert!(now() < until, "inter-Hart acknowledgement timed out");
        spin_loop();
    }
}

fn minor(hart: usize, hertz: usize) -> ! {
    assert_eq!(unsafe { TRAP.install() }, Ok(()));
    assert_eq!(unsafe { TRAP.install() }, Err(InstallError::Occupied));
    let mut receiver = unsafe { Receiver::admit(hart) }.unwrap();
    assert_eq!(receiver.pending(), Ok(false));
    assert_eq!(receiver.acknowledge(), Err(Error::Busy));
    assert_eq!(
        unsafe { receiver.wait(|| panic!("closed source")) },
        Err(Error::Busy)
    );
    receiver.open().unwrap();
    assert_eq!(
        ORDINARY.acquire(|_| unsafe { receiver.wait(|| panic!("occupied protection")) }),
        Ok(Err(Error::Busy)),
    );
    unsafe { *RECEPTION.owner().0.get() = Some(receiver) };
    READY.store(1, Release);
    for round in 1..=2 {
        {
            let receiver = unsafe { (&*RECEPTION.owner().0.get()).as_ref().unwrap() };
            if round == 1 {
                MAILBOX
                    .wait_for_issues(round, |state, expected| wait(state, expected, hertz as u64));
                assert_eq!(
                    unsafe { receiver.wait(|| MAILBOX.ready(round)) },
                    Ok(Wake::Ready)
                );
                assert_eq!(unsafe { receiver.wait(|| false) }, Ok(Wake::Pending));
            } else {
                let limit = now() + 5 * hertz as u64;
                let mut announced = false;
                loop {
                    let wake = unsafe {
                        receiver.wait(|| {
                            if !announced {
                                assert!(!MAILBOX.ready(round));
                                FINAL_CHECK.store(round, Release);
                                announced = true;
                                return false;
                            }
                            MAILBOX.ready(round)
                        })
                    }
                    .unwrap();
                    if wake != Wake::Recheck {
                        break;
                    }
                    assert!(now() < limit, "idle wake timed out");
                }
            }
            assert_eq!(CAPTURES.load(Relaxed), round - 1);
        }
        receive(round, now() + 5 * hertz as u64);
        assert_eq!(
            ORDINARY.acquire(|value| {
                *value += 1;
                *value
            }),
            Ok(round),
        );
        MAILBOX.recheck(round, || {
            let receiver = unsafe { (&mut *RECEPTION.owner().0.get()).as_mut().unwrap() };
            assert_eq!(receiver.pending(), Ok(false));
            assert_eq!(receiver.acknowledge(), Err(Error::Busy));
            if round == 2 {
                unsafe { receiver.shut() }.unwrap();
            }
        });
    }
    loop {
        spin_loop();
    }
}

pub fn check(boot: Boot<'_>) {
    assert!(!MINOR.ready());
    assert_eq!(READY.load(Acquire), 0);

    let cpus = boot.riscv_cpus();
    assert!(cpus.cpu(0).unwrap().unwrap().enabled());
    assert!(cpus.cpu(1).unwrap().unwrap().enabled());
    assert!(cpus.cpu(2).unwrap().is_none());
    let source = boot.processor();
    let peer = (0..2).find(|hart| *hart != source).unwrap();
    let timebase = RiscvTimebase::describe(cpus).unwrap().unwrap();
    let counter = unsafe { Counter::admit(timebase, source as u64) }.unwrap();
    let hertz = counter.hertz() as usize;
    let sbi = boot.sbi_services();
    let harts = sbi.hart_state_management().unwrap().unwrap();
    let ipi = sbi.ipi().unwrap().unwrap();
    let mut send_state = SendState::FRESH;
    let local = Target::describe(&cpus.cpu(source as u64).unwrap().unwrap()).unwrap();
    let target = Target::describe(&cpus.cpu(peer as u64).unwrap().unwrap()).unwrap();
    {
        let mut sender =
            unsafe { Sender::bind(cpus, source, &mut send_state, harts, ipi) }.unwrap();
        assert!(matches!(
            unsafe { sender.bind_ipi(&local) },
            Err(Error::Scope)
        ));
    }
    unsafe { MINOR.start(peer, minor, hertz, &mut Startup::Sbi(harts)) }.unwrap();
    wait(&READY, 1, hertz as u64);
    for round in 1..=2 {
        if round == 2 {
            wait(&FINAL_CHECK, round, hertz as u64);
            let after_check = now() + hertz as u64 / 100;
            while now() < after_check {
                spin_loop();
            }
        }
        let mut sender =
            unsafe { Sender::bind(cpus, source, &mut send_state, harts, ipi) }.unwrap();
        let mut route = unsafe { sender.bind_ipi(&target) }.unwrap();
        MAILBOX.publish(&mut route, round, round == 1);
        MAILBOX.wait_for_recheck(round, |state, expected| wait(state, expected, hertz as u64));
    }
    boot_diagnostic::write(
        "PASS riscv64-ipi original-hart=1 issued=3 captured=2 resumed=2 rechecked=2 cleared=2 shut=1 idle=2\n",
    );
}
