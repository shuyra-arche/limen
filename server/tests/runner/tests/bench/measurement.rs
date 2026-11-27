use super::decode;

const HEADER: &str = "LIMEN_BENCH_V2 0 10000000 1\n";
const SERIES: &str = "LIMEN_BENCH_SERIES clock-sample 1 1024 16384 2\n";
const FIRST: &str = "LIMEN_BENCH_SAMPLE 0 10000\n";
const SECOND: &str = "LIMEN_BENCH_SAMPLE 1 11000\n";

#[test]
fn complete_measurements_preserve_raw_samples() {
    let log = format!("boot diagnostics\n{HEADER}{SERIES}{FIRST}{SECOND}other diagnostics\n");
    let result = decode(&log).unwrap();
    assert_eq!(result.environment, "qemu");
    assert_eq!(result.hart, 0);
    assert_eq!(result.frequency_hz, 10_000_000);
    assert_eq!(result.series.len(), 1);
    let series = &result.series[0];
    assert_eq!(series.name, "clock-sample");
    assert_eq!(series.participants, 1);
    assert_eq!(series.warmup_iterations, 1024);
    assert_eq!(series.iterations, 16_384);
    assert_eq!(series.elapsed_ns, [10_000, 11_000]);
}

#[test]
fn incomplete_or_ambiguous_measurements_are_rejected() {
    for invalid in [
        "PASS\n".into(),
        format!("{HEADER}{SERIES}{FIRST}"),
        format!("{HEADER}{SERIES}{SECOND}{FIRST}"),
        format!("{HEADER}{SERIES}{FIRST}{FIRST}"),
        format!("{HEADER}{SERIES}{FIRST}{SECOND}{SECOND}"),
        format!("{HEADER}{HEADER}{SERIES}{FIRST}{SECOND}"),
        format!("{HEADER}{FIRST}{SERIES}{SECOND}"),
        format!("{SERIES}{HEADER}{FIRST}{SECOND}"),
        format!("{HEADER}{SERIES}{FIRST}{}", SECOND.trim_end()),
        format!("{HEADER}{SERIES}{FIRST}{SECOND}LIMEN_BENCH_SAMPLE bad\n"),
        format!("{HEADER}{SERIES}{FIRST}LIMEN_BENCH_SAMPLE 1 0\n"),
        format!("{HEADER}{SERIES}{FIRST}LIMEN_BENCH_SAMPLE 1 11000 extra\n"),
    ] {
        assert!(decode(&invalid).is_err(), "{invalid}");
    }
}

#[test]
fn scale_and_capacity_are_checked_before_collection() {
    for header in [
        "LIMEN_BENCH_V2 0 0 1\n",
        "LIMEN_BENCH_V2 0 10000000 0\n",
        "LIMEN_BENCH_V2 0 10000000 17\n",
    ] {
        assert!(decode(&format!("{header}{SERIES}{FIRST}{SECOND}")).is_err());
    }
    for series in [
        "LIMEN_BENCH_SERIES clock-sample 0 1024 16384 2\n",
        "LIMEN_BENCH_SERIES clock-sample 65 1024 16384 2\n",
        "LIMEN_BENCH_SERIES clock-sample 1 1024 0 2\n",
        "LIMEN_BENCH_SERIES clock-sample 1 1024 16384 0\n",
        "LIMEN_BENCH_SERIES clock-sample 1 1024 16384 65\n",
        "LIMEN_BENCH_SERIES clock-sample 1 1024 16384 2 extra\n",
    ] {
        assert!(decode(&format!("{HEADER}{series}{FIRST}{SECOND}")).is_err());
    }
}

#[test]
fn all_declared_series_must_be_distinct_and_complete() {
    let header = "LIMEN_BENCH_V2 0 10000000 2\n";
    let peer = "LIMEN_BENCH_SERIES turn-shared 4 4096 65536 2\n";
    let complete = format!("{header}{SERIES}{FIRST}{SECOND}{peer}{FIRST}{SECOND}");
    let result = decode(&complete).unwrap();
    assert_eq!(result.series_count, 2);
    assert_eq!(result.series[1].participants, 4);
    assert_eq!(result.series[1].iterations, 65_536);
    for invalid in [
        format!("{header}{SERIES}{FIRST}{SECOND}"),
        format!("{header}{SERIES}{FIRST}{peer}{FIRST}{SECOND}"),
        format!("{header}{SERIES}{FIRST}{SECOND}{peer}{FIRST}"),
        format!("{header}{SERIES}{FIRST}{SECOND}{SERIES}{FIRST}{SECOND}"),
        format!("{complete}{peer}{FIRST}{SECOND}"),
    ] {
        assert!(decode(&invalid).is_err(), "{invalid}");
    }
}
