//! Direct STIMECMP access granted by the QEMU/OpenSBI supervisor handoff.
use kernel::{
    entry::boot::{self, Boot},
    metal::{
        chrono::riscv64::{Counter, RiscvTimebase},
        interrupt::internal::{
            sstc::{Failure, State, Timer},
            supervisor::Reception,
        },
        processor::execution::description::riscv64::RiscvIsa,
    },
};

pub(super) fn check(boot: &Boot<'_>, local: &mut Reception) {
    let hart = boot.hart() as u64;
    let cpus = boot.riscv_cpus();
    let cpu = cpus.cpu(hart).unwrap().unwrap();
    let isa = RiscvIsa::describe(&cpu).unwrap().unwrap();
    let timebase = RiscvTimebase::describe(cpus).unwrap().unwrap();
    let counter = unsafe { Counter::admit(timebase, hart) }.unwrap();
    let mut timer = unsafe { Timer::admit(&cpu, isa, &counter, local) }.unwrap();
    assert!(matches!(timer.state(), State::Inherited(_)));
    let limit = counter.read().unwrap() + u64::from(counter.hertz());
    loop {
        match timer.shut() {
            Err(Failure::Busy) if counter.read().unwrap() < limit => continue,
            result => result.unwrap(),
        }
        break;
    }
    assert_eq!(timer.program(0), Err(Failure::Range));
    for _ in 0..2 {
        let now = counter.read().unwrap();
        let limit = now + u64::from(counter.hertz()) * 2;
        timer.program(limit).unwrap();
        timer
            .program(now + u64::from(counter.hertz()) / 20)
            .unwrap();
        assert_eq!(
            unsafe { super::reception::receive(limit) },
            (1usize << 63) | 5
        );
        assert_eq!(timer.due(), Ok(true));
    }
    let limit = counter.read().unwrap() + u64::from(counter.hertz());
    loop {
        match timer.shut() {
            Err(Failure::Busy) if counter.read().unwrap() < limit => continue,
            result => result.unwrap(),
        }
        break;
    }
    assert_eq!(timer.state(), State::Idle);
    boot::write("PASS sstc expired=2 replaced=1 shut=1\n");
}
