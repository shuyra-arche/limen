use super::*;

#[test]
fn domain_readback_distinguishes_signature_mode_order_and_enable() {
    let shut = DomainConfiguration::shut();
    let open = DomainConfiguration::open();
    assert_eq!(shut.write_bits(), 0);
    assert_eq!(open.write_bits(), 0x100);
    assert!(shut.confirms(0x8000_0000));
    assert!(open.confirms(0x8000_0100));
    for invalid in [0, 0x80, 0x8000_0001, 0x8000_0004, 0x8000_0080, 0x8100_0000] {
        assert!(!shut.confirms(invalid));
    }
    assert!(!open.confirms(0x8000_0000));
    assert!(!shut.confirms(0x8000_0100));
    assert!(matches!(
        DomainConfiguration::read(0x8000_0004),
        Some(DomainConfiguration { delivery: Delivery::Msi, .. })
    ));
}

#[test]
fn direct_source_target_and_claim_have_distinct_encodings() {
    for (mode, bits) in [
        (Mode::EdgeRising, 4),
        (Mode::EdgeFalling, 5),
        (Mode::LevelHigh, 6),
        (Mode::LevelLow, 7),
    ] {
        let config = SourceConfiguration::Active(mode);
        assert_eq!(config.bits(), bits);
        assert!(config.confirms(bits));
        assert!(!config.confirms(bits | (1 << 10)));
    }
    assert!(SourceConfiguration::Inactive.confirms(0));
    let target = Target::new(3).unwrap();
    assert_eq!(target.bits(), 0x000c_0001);
    assert!(target.confirms(0x000c_0001));
    assert!(!target.confirms(0x000c_0101));
    assert!(Target::new(1 << 14).is_none());
    assert_eq!(Interrupt::read(0), Some(Interrupt::Empty));
    assert_eq!(
        Interrupt::read(0x000a_0001),
        Some(Interrupt::Claimed { source: 10, priority: 1 })
    );
    for raw in [1, 0x0001_0000, 0x0401_0001, 0x0001_0101] {
        assert_eq!(Interrupt::read(raw), None);
    }
}
