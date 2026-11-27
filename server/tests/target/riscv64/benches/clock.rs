//! A measurement transport smoke benchmark over the admitted local timebase.
#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(guest::scenario)]
#![reexport_test_harness_main = "collected"]

use core::hint::black_box;
use guest::{bench, platform};
use kernel::metal::chrono::riscv64::{description::RiscvTimebase, time::Counter};

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
            *sample = bench::measure(clock, || {
                for _ in 0..ITERATIONS {
                    let _ = black_box(clock.sample().unwrap());
                }
            });
        }

        bench::report(clock, &[bench::Series {
            name: "clock-sample",
            participants: 1,
            warmup_iterations: WARMUP,
            iterations: ITERATIONS,
            elapsed_ns: &elapsed,
        }]);
        guest::finish()
    }
}
