#![no_std]
#![no_main]

use core::{
    arch::asm,
    fmt::Write,
    hint::spin_loop,
    sync::atomic::{
        AtomicUsize,
        Ordering::{Acquire, Release},
    },
};
use kernel::{
    entry::boot::{self, Boot, Minor, Startup},
    metal::{
        chrono::riscv64::{Counter, RiscvTimebase},
        interrupt::riscv64::internal::ipi::{Error, Receiver, Sender, Target},
    },
};

#[path = "../../../../support/riscv64/reception.rs"]
mod reception;
#[path = "../../../../support/ipi.rs"]
mod ipi;

static MINOR: Minor = Minor::new();
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
        MAILBOX.wait_for_issues(round, |state, expected| wait(state, expected, hertz as u64));
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

#[unsafe(no_mangle)]
extern "Rust" fn limen_platform_ready(boot: Boot<'_>) -> ! {
    let cpus = boot.riscv_cpus();
    let source = boot.hart();
    let peer = (0..2).find(|hart| *hart != source).unwrap();
    let timebase = RiscvTimebase::describe(cpus).unwrap().unwrap();
    let counter = unsafe { Counter::admit(timebase, source as u64) }.unwrap();
    let hertz = counter.hertz() as usize;
    let sbi = boot.sbi_services();
    let harts = sbi.hart_state_management().unwrap().unwrap();
    let ipi = sbi.ipi().unwrap().unwrap();
    let mut sender = Sender::bind(cpus, source, harts, ipi).unwrap();
    let local = Target::describe(&cpus.cpu(source as u64).unwrap().unwrap()).unwrap();
    let target = Target::describe(&cpus.cpu(peer as u64).unwrap().unwrap()).unwrap();
    assert_eq!(sender.notify(&local), Err(Error::Scope));
    unsafe { MINOR.start(peer, minor, hertz, &mut Startup::Sbi(harts)) }.unwrap();
    wait(&READY, 1, hertz as u64);
    let mut route = unsafe { sender.bind_ipi(&target) }.unwrap();
    for round in 1..=2 {
        MAILBOX.publish(&mut route, round, round == 1);
        MAILBOX.wait_for_recheck(round, |state, expected| wait(state, expected, hertz as u64));
    }
    boot::write("PASS rv64-ipi original-hart=1 issued=3 rechecked=2 cleared=2 shut=1\n");
    boot::shutdown(true)
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    if let Some(mut console) = boot::console() {
        let _ = writeln!(console, "FAIL RV64 IPI: {info}");
    }
    boot::shutdown(false)
}
