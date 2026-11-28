use super::frequency;

#[test]
fn hpet_nominal_rate_is_checked_and_not_rounded() {
    assert_eq!(frequency(10_000_000).unwrap().get(), 100_000_000);
    assert_eq!(frequency(100_000_000).unwrap().get(), 10_000_000);
    assert_eq!(frequency(0), None);
    assert_eq!(frequency(100_000_001), None);
    assert_eq!(frequency(69_841_279), None);
}
