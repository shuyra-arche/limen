use super::frequency;

#[test]
fn tsc_rate_requires_the_complete_exact_crystal_ratio() {
    assert_eq!(frequency(3, 250, 24_000_000).unwrap().get(), 2_000_000_000);
    assert_eq!(frequency(2, 350, 25_000_000).unwrap().get(), 4_375_000_000);
    assert_eq!(
        frequency(1, u32::MAX, u32::MAX).unwrap().get(),
        u64::from(u32::MAX).pow(2)
    );
    assert_eq!(frequency(0, 1, 1), None);
    assert_eq!(frequency(1, 0, 1), None);
    assert_eq!(frequency(1, 1, 0), None);
    assert_eq!(frequency(3, 1, 10), None);
}
