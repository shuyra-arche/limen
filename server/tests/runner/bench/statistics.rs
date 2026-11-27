//! Descriptive statistics over complete, validated raw batch observations.
use super::measurement::{Measurement, Series};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub(super) struct Summary<'a> {
    name: &'a str,
    participants: usize,
    operations_per_batch: u64,
    samples: usize,
    min_batch_ns: u64,
    pub(super) median_batch_ns: f64,
    max_batch_ns: u64,
    mad_batch_ns: f64,
    median_ns_per_operation: f64,
    median_operations_per_second: f64,
}

fn median(sorted: &[f64]) -> f64 {
    let middle = sorted.len() / 2;
    if sorted.len().is_multiple_of(2) {
        (sorted[middle - 1] + sorted[middle]) / 2.0
    } else {
        sorted[middle]
    }
}

pub(super) fn series(series: &Series) -> Summary<'_> {
    let mut sorted: Vec<_> = series.elapsed_ns.iter().map(|&ns| ns as f64).collect();
    sorted.sort_unstable_by(f64::total_cmp);
    let center = median(&sorted);
    let mut deviations: Vec<_> = sorted.iter().map(|ns| (ns - center).abs()).collect();
    deviations.sort_unstable_by(f64::total_cmp);
    let operations = series.iterations as f64;
    Summary {
        name: &series.name,
        participants: series.participants,
        operations_per_batch: series.iterations,
        samples: sorted.len(),
        min_batch_ns: *series.elapsed_ns.iter().min().unwrap(),
        median_batch_ns: center,
        max_batch_ns: *series.elapsed_ns.iter().max().unwrap(),
        mad_batch_ns: median(&deviations),
        median_ns_per_operation: center / operations,
        median_operations_per_second: operations * 1e9 / center,
    }
}

pub(super) fn summarize(measurement: &Measurement) -> Vec<Summary<'_>> {
    measurement.series.iter().map(series).collect()
}

#[cfg(test)]
#[path = "../tests/bench/statistics.rs"]
mod tests;
