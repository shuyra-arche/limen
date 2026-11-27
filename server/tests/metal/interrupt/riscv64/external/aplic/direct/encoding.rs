use super::*;

#[test]
fn direct_target_and_claim_have_distinct_encodings() {
    let target = Target::new(3).unwrap();
    assert_eq!(target.bits(), 0x000c_0001);
    assert!(target.confirms(0x000c_0001));
    assert!(!target.confirms(0x000c_0101));
    assert!(Target::new(1 << 14).is_none());
    assert_eq!(Interrupt::read(0), Some(Interrupt::Empty));
    assert_eq!(
        Interrupt::read(0x000a_0001),
        Some(Interrupt::Claimed {
            source: 10,
            priority: 1
        })
    );
    for raw in [1, 0x0001_0000, 0x0401_0001, 0x0001_0101] {
        assert_eq!(Interrupt::read(raw), None);
    }
}
