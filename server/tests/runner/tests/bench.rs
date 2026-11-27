use super::decode;

const HEADER: &str = "LIMEN_BENCH_V1 clock-sample 0 10000000 16384 2\n";
const FIRST: &str = "LIMEN_BENCH_SAMPLE 0 10000\n";
const SECOND: &str = "LIMEN_BENCH_SAMPLE 1 11000\n";

#[test]
fn complete_measurements_preserve_raw_samples() {
    let log = format!("boot diagnostics\n{HEADER}{FIRST}{SECOND}other diagnostics\n");
    let result = decode(&log).unwrap();
    assert_eq!(result.environment, "qemu");
    assert_eq!(result.name, "clock-sample");
    assert_eq!(result.hart, 0);
    assert_eq!(result.frequency_hz, 10_000_000);
    assert_eq!(result.iterations, 16_384);
    assert_eq!(result.elapsed_ns, [10_000, 11_000]);
}

#[test]
fn incomplete_or_ambiguous_measurements_are_rejected() {
    for invalid in [
        "PASS\n".into(),
        format!("{HEADER}{FIRST}"),
        format!("{HEADER}{SECOND}{FIRST}"),
        format!("{HEADER}{FIRST}{FIRST}"),
        format!("{HEADER}{FIRST}{SECOND}{SECOND}"),
        format!("{HEADER}{HEADER}{FIRST}{SECOND}"),
        format!("{FIRST}{HEADER}{SECOND}"),
        format!("{HEADER}{FIRST}{}", SECOND.trim_end()),
        format!("{HEADER}{FIRST}{SECOND}LIMEN_BENCH_SAMPLE bad\n"),
        format!("{HEADER}{FIRST}LIMEN_BENCH_SAMPLE 1 0\n"),
        format!("{HEADER}{FIRST}LIMEN_BENCH_SAMPLE 1 11000 extra\n"),
    ] {
        assert!(decode(&invalid).is_err(), "{invalid}");
    }
}

#[test]
fn scale_and_sample_capacity_are_checked_before_collection() {
    for header in [
        "LIMEN_BENCH_V1 clock-sample 0 0 16384 2\n",
        "LIMEN_BENCH_V1 clock-sample 0 10000000 0 2\n",
        "LIMEN_BENCH_V1 clock-sample 0 10000000 16384 0\n",
        "LIMEN_BENCH_V1 clock-sample 0 10000000 16384 65\n",
        "LIMEN_BENCH_V1 clock-sample 0 10000000 16384 2 extra\n",
    ] {
        assert!(decode(&format!("{header}{FIRST}{SECOND}")).is_err());
    }
}
