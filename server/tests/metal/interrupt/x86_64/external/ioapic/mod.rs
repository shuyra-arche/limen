use super::*;

#[test]
fn version_and_gsi_bounds_protect_the_selector_window() {
    let pins = Pins::probe(24, 0x0017_0011).unwrap();
    assert_eq!(pins.pin(23), Err(Failure::Scope));
    assert_eq!(pins.pin(24), Ok(0));
    assert_eq!(pins.pin(47), Ok(23));
    assert_eq!(pins.pin(48), Err(Failure::Scope));
    assert!(Pins::probe(u32::MAX - 22, 0x0017_0020).is_none());
    assert!(Pins::probe(24, 0x0078_0020).is_none());
    assert!(Pins::probe(0, 0xffff_ffff).is_none());
}

#[test]
fn fixed_routes_never_echo_remote_irr_or_delivery_status() {
    let route = Route {
        gsi: 16,
        vector: 0x51,
        hart: 3,
        active_low: true,
        level: true,
    };
    assert_eq!(route.high(), 0x0300_0000);
    assert_eq!(route.low(true), 0x1_a051);
    assert_eq!(route.low(false), 0xa051);
    assert_eq!(route.low(true) & (SENDING | REMOTE_IRR), 0);
}
