use kernel::metal::{
    processor::treaty::{LocalStorage, Stack},
    sync::treaty::Supply,
};

use kernel::metal::platform::boot::treaty as boot_diagnostic;
use kernel::metal::processor::treaty::{Minor, Startup};

use core::{
    arch::asm,
    hint::spin_loop,
    sync::atomic::{
        AtomicUsize,
        Ordering::{Acquire, Release},
    },
};
use kernel::{
    entry::boot::Boot,
    metal::{
        chrono::riscv64::{description::RiscvTimebase, time::Counter},
        interrupt::riscv64::internal::ipi::{Error, Receiver, SendState, Sender, Target},
    },
};

#[path = "../../../../target/support/ipi.rs"]
mod ipi;
#[path = "../../../../target/support/riscv64/reception.rs"]
mod reception;

static MINOR_STACK: Stack<{ 32 * 1024 }> = Stack::new();
static MINOR_LOCAL: LocalStorage<5> = LocalStorage::new(Supply::new());
static MINOR: Minor = Minor::new(&MINOR_STACK, &MINOR_LOCAL);
static READY: AtomicUsize = AtomicUsize::new(0);
static MAILBOX: ipi::Mailbox = ipi::Mailbox::new();

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
    let mut receiver = unsafe { Receiver::admit(hart) }.unwrap();
    assert_eq!(receiver.pending(), Ok(false));
    assert_eq!(receiver.acknowledge(), Err(Error::Busy));
    receiver.open().unwrap();
    READY.store(1, Release);
    for round in 1..=2 {
        if round == 1 {
            MAILBOX.wait_for_issues(round, |state, expected| wait(state, expected, hertz as u64));
        }
        assert_eq!(
            unsafe { reception::receive(now() + 5 * hertz as u64) },
            (1usize << 63) | 1
        );
        assert_eq!(receiver.pending(), Ok(true));
        MAILBOX.recheck(round, || {
            receiver.acknowledge().unwrap();
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
    let source = boot.hart();
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
        let mut sender =
            unsafe { Sender::bind(cpus, source, &mut send_state, harts, ipi) }.unwrap();
        let mut route = unsafe { sender.bind_ipi(&target) }.unwrap();
        MAILBOX.publish(&mut route, round, round == 1);
        MAILBOX.wait_for_recheck(round, |state, expected| wait(state, expected, hertz as u64));
    }
    boot_diagnostic::write(
        "PASS riscv64-ipi original-hart=1 issued=3 rechecked=2 cleared=2 shut=1\n",
    );
}
