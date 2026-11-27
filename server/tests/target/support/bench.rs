//! Measurement boundaries and the shared guest reporting contract.
use core::{
    fmt::Write,
    sync::atomic::{Ordering, fence},
};
use kernel::metal::{chrono::treaty::Clock, platform::boot::treaty::console};

pub struct Series<'a> {
    pub name: &'a str,
    pub participants: usize,
    pub warmup_iterations: usize,
    pub iterations: usize,
    pub elapsed_ns: &'a [u64],
}

/// Measure one complete batch, including boundary ordering overhead.
pub fn measure(clock: &Clock, work: impl FnOnce()) -> u64 {
    fence(Ordering::SeqCst);
    let start = clock.sample().expect("local counter");
    fence(Ordering::SeqCst);
    work();
    fence(Ordering::SeqCst);
    let stop = clock.sample().expect("local counter");
    fence(Ordering::SeqCst);
    let elapsed = stop.elapsed_since(start).expect("monotonic counter");
    let nanos = u64::try_from(elapsed.as_nanos()).unwrap();
    assert_ne!(nanos, 0, "measurement below counter resolution");
    nanos
}

/// Emit a complete declared series set after all timed work has settled.
pub fn report(clock: &Clock, series: &[Series<'_>]) {
    assert!(!series.is_empty());
    let mut console = console().expect("benchmark measurement channel");
    writeln!(
        console,
        "LIMEN_BENCH_V2 {} {} {}",
        clock.hart(),
        clock.frequency(),
        series.len()
    )
    .unwrap();
    for series in series {
        writeln!(
            console,
            "LIMEN_BENCH_SERIES {} {} {} {} {}",
            series.name,
            series.participants,
            series.warmup_iterations,
            series.iterations,
            series.elapsed_ns.len()
        )
        .unwrap();
        for (index, value) in series.elapsed_ns.iter().enumerate() {
            writeln!(console, "LIMEN_BENCH_SAMPLE {index} {value}").unwrap();
        }
    }
}
