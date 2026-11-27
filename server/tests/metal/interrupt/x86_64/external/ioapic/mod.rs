use super::{
    Failure,
    pin::{Pin, Pins},
    route::{Route, quiesced},
};

#[test]
fn version_and_gsi_bounds_protect_the_selector_window() {
    let pins = Pins::probe(24, 0x0017_0011).unwrap();
    assert_eq!(pins.pin(23).map(Pin::index), Err(Failure::Scope));
    assert_eq!(pins.pin(24).map(Pin::index), Ok(0));
    assert_eq!(pins.pin(47).map(Pin::index), Ok(23));
    assert_eq!(pins.pin(48).map(Pin::index), Err(Failure::Scope));
    let last = Pins::probe(0, 0x0077_0020).unwrap().pin(119).unwrap();
    assert_eq!(last.low_register(), 254);
    assert_eq!(last.high_register(), 255);
    assert!(Pins::probe(u32::MAX - 22, 0x0017_0020).is_none());
    assert!(Pins::probe(24, 0x0078_0020).is_none());
    assert!(Pins::probe(0, 0xffff_ffff).is_none());
}

#[test]
fn fixed_routes_never_echo_remote_irr_or_delivery_status() {
    let route = Route {
        gsi: 16,
        vector: 0x51,
        processor: 3,
        active_low: true,
        level: true,
    };
    assert_eq!(route.high(), 0x0300_0000);
    assert_eq!(route.low(true), 0x1_a051);
    assert_eq!(route.low(false), 0xa051);
    assert!(quiesced(route.low(true)));
    assert!(!quiesced(route.low(true) | (1 << 14)));
    assert!(!quiesced(route.low(true) | (1 << 12)));
}
