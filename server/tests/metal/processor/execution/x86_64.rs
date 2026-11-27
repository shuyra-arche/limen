use super::{StateControl, base_xcr0};

#[test]
fn x86_cooperative_profile_checks_each_enabled_component() {
    let edx = 7 << 24;
    let fxsr = 1 << 9;
    let extended = fxsr | (1 << 18);
    assert_eq!(
        StateControl::from_registers(edx, 0, 0, fxsr),
        Some(StateControl::Legacy)
    );
    for bit in [24, 25, 26] {
        assert_eq!(
            StateControl::from_registers(edx ^ (1 << bit), 0, 0, fxsr),
            None
        );
    }
    for cr0 in [1 << 2, 1 << 3] {
        assert_eq!(StateControl::from_registers(edx, 0, cr0, fxsr), None);
    }
    assert_eq!(StateControl::from_registers(edx, 0, 0, 0), None);
    for bits in 0..4 {
        let expected = (bits == 3).then_some(StateControl::Xcr0);
        assert_eq!(
            StateControl::from_registers(edx, bits << 26, 0, extended),
            expected
        );
    }
    assert_eq!(StateControl::from_registers(edx, 3 << 26, 0, fxsr), None);
    assert!(base_xcr0(3));
    for bits in [0, 1, 2, 7, (1 << 32) | 3] {
        assert!(!base_xcr0(bits));
    }
}
