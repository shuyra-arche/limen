//! A measurement transport smoke benchmark over the admitted local timebase.
#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(guest::scenario)]
#![reexport_test_harness_main = "collected"]

use core::{fmt::Write, hint::black_box};
use guest::platform;
use kernel::metal::{
    chrono::riscv64::{description::RiscvTimebase, time::Counter},
    platform::boot::treaty::console,
};

const WARMUP: usize = 1024;
const ITERATIONS: usize = 16_384;
const SAMPLES: usize = 9;

guest::entry! {
    platform: platform::Platform::native(1),
    run: |boot| {
        let timebase = RiscvTimebase::describe(boot.riscv_cpus()).unwrap().unwrap();
        let counter = unsafe { Counter::admit(timebase, boot.hart() as u64) }.unwrap();
        let clock = counter.clock();
        assert_eq!(clock.hart(), boot.hart());

        for _ in 0..WARMUP {
            let _ = black_box(clock.sample().unwrap());
        }
        let mut elapsed = [0_u64; SAMPLES];
        for sample in &mut elapsed {
            let start = clock.sample().unwrap();
            for _ in 0..ITERATIONS {
                let _ = black_box(clock.sample().unwrap());
            }
            let stop = clock.sample().unwrap();
            let duration = stop.elapsed_since(start).expect("local monotonic observation");
            *sample = u64::try_from(duration.as_nanos()).unwrap();
            assert_ne!(*sample, 0, "measurement below counter resolution");
        }

        // Keep reporting outside the timed region.
        let mut console = console().expect("benchmark measurement channel");
        writeln!(
            console,
            "LIMEN_BENCH_V1 clock-sample {} {} {ITERATIONS} {SAMPLES}",
            clock.hart(),
            clock.frequency()
        )
        .unwrap();
        for (index, value) in elapsed.into_iter().enumerate() {
            writeln!(console, "LIMEN_BENCH_SAMPLE {index} {value}").unwrap();
        }
        guest::finish()
    }
}
