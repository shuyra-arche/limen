//! Direct STIMECMP access granted by the QEMU/OpenSBI supervisor handoff.
use core::time::Duration;
use kernel::metal::interrupt::treaty::{AbsoluteTimer, TimerEffect};
use kernel::metal::platform::boot::treaty as boot_diagnostic;
use kernel::{
    entry::boot::Boot,
    metal::{
        chrono::riscv64::{description::RiscvTimebase, time::Counter},
        interrupt::riscv64::internal::{
            supervisor::Reception,
            timer::{Failure, State, Timer},
        },
        processor::description::riscv64::isa::RiscvIsa,
    },
};

#[path = "../../../../chrono/observation.rs"]
mod clock;
#[path = "../../../timer.rs"]
mod consumer;

pub(super) fn check(boot: &Boot<'_>, local: &mut Reception) {
    let hart = boot.processor() as u64;
    let cpus = boot.riscv_cpus();
    let cpu = cpus.cpu(hart).unwrap().unwrap();
    let isa = RiscvIsa::describe(&cpu).unwrap().unwrap();
    let timebase = RiscvTimebase::describe(cpus).unwrap().unwrap();
    let counter = unsafe { Counter::admit(timebase, hart) }.unwrap();
    clock::observe(counter.clock());
    let mut timer = unsafe { Timer::admit_sstc(&cpu, isa, &counter, local) }.unwrap();
    assert!(matches!(timer.state(), State::Inherited(Some(_))));
    let limit = counter.read().unwrap() + u64::from(counter.hertz());
    loop {
        let result = if timer.pending_effect().unwrap() == Some(TimerEffect::Clearing) {
            timer.confirm_effect()
        } else {
            unsafe { timer.clear() }
        };
        match result {
            Err(Failure::Busy) if counter.read().unwrap() < limit => continue,
            result => result.unwrap(),
        }
        break;
    }
    assert_eq!(timer.arm_at(counter.deadline(0)), Err(Failure::Range));
    for round in 0..2 {
        let now = counter.read().unwrap();
        let limit = now + u64::from(counter.hertz()) * 2;
        if round == 0 {
            consumer::arm(counter.clock(), &mut timer, Duration::from_secs(2));
        } else {
            unsafe { timer.rearm(counter.deadline(limit)) }.unwrap();
        }
        assert_eq!(timer.due(), Ok(false));
        consumer::advance(counter.clock(), &mut timer, Duration::from_millis(50));
        assert_eq!(
            unsafe { super::reception::receive(limit) },
            (1usize << 63) | 5
        );
        consumer::confirm_due(&timer);
    }
    let limit = counter.read().unwrap() + u64::from(counter.hertz());
    loop {
        match unsafe { consumer::stop(&mut timer) } {
            Err(Failure::Busy) if counter.read().unwrap() < limit => continue,
            result => result.unwrap(),
        }
        break;
    }
    assert_eq!(timer.state(), State::Idle);
    boot_diagnostic::write("PASS sstc expired=2 replaced=1 shut=1 common=1\n");
}
