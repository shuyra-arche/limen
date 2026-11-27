use super::super::{Series, statistics};

fn measured(samples: &[u64]) -> Series {
    Series {
        name: "fixture".into(),
        participants: 2,
        warmup_iterations: 10,
        iterations: 100,
        sample_count: samples.len(),
        elapsed_ns: samples.to_vec(),
    }
}

#[test]
fn summary_preserves_batch_units_and_robust_statistics() {
    let series = measured(&[400, 200, 100, 300, 10_000]);
    let result = serde_json::to_value(statistics::series(&series)).unwrap();
    assert_eq!(result["min_batch_ns"], 100);
    assert_eq!(result["median_batch_ns"].as_f64(), Some(300.0));
    assert_eq!(result["max_batch_ns"], 10_000);
    assert_eq!(result["mad_batch_ns"].as_f64(), Some(100.0));
    assert_eq!(result["median_ns_per_operation"].as_f64(), Some(3.0));
    assert_eq!(series.elapsed_ns, [400, 200, 100, 300, 10_000]);
}

#[test]
fn even_median_does_not_truncate_or_overflow_integer_counts() {
    let series = measured(&[1, 2]);
    let result = statistics::series(&series);
    assert_eq!(result.median_batch_ns, 1.5);
    let extreme = measured(&[u64::MAX - 1, u64::MAX]);
    assert!(statistics::series(&extreme).median_batch_ns.is_finite());
}
