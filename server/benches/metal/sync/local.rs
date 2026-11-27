use guest::{Boot, bench};
use kernel::metal::{
    chrono::{
        riscv64::{description::RiscvTimebase, time::Counter},
        treaty::Clock,
    },
    sync::treaty::{LeafLock, TurnLock},
};

const WARMUP: usize = 1024;
const ITERATIONS: usize = 16_384;
const SAMPLES: usize = 9;

fn measure(clock: &Clock, mut increment: impl FnMut()) -> [u64; SAMPLES] {
    for _ in 0..WARMUP {
        increment();
    }
    core::array::from_fn(|_| {
        bench::measure(clock, || {
            for _ in 0..ITERATIONS {
                increment();
            }
        })
    })
}

pub fn check(boot: Boot<'_>) {
    let hart = boot.hart();
    assert_eq!(hart, 0);
    assert!(boot.riscv_cpus().cpu(1).unwrap().is_none());
    let timebase = RiscvTimebase::describe(boot.riscv_cpus()).unwrap().unwrap();
    let counter = unsafe { Counter::admit(timebase, hart as u64) }.unwrap();
    let clock = counter.clock();

    let leaf = LeafLock::new(0_usize);
    let turn = TurnLock::new(0_usize);
    let leaf_elapsed = measure(clock, || leaf.acquire(|value| *value += 1).unwrap());
    let turn_elapsed = measure(clock, || turn.acquire(|value| *value += 1).unwrap());
    let total = WARMUP + SAMPLES * ITERATIONS;
    assert_eq!(leaf.acquire(|value| *value), Ok(total));
    assert_eq!(turn.acquire(|value| *value), Ok(total));

    bench::report(
        clock,
        &[
            bench::Series {
                name: "leaf-local",
                participants: 1,
                warmup_iterations: WARMUP,
                iterations: ITERATIONS,
                elapsed_ns: &leaf_elapsed,
            },
            bench::Series {
                name: "turn-local",
                participants: 1,
                warmup_iterations: WARMUP,
                iterations: ITERATIONS,
                elapsed_ns: &turn_elapsed,
            },
        ],
    );
}
