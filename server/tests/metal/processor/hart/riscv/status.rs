use super::*;

#[test]
fn global_mask_and_each_extension_state_are_independent() {
    let states = [
        Extension::Off,
        Extension::Initial,
        Extension::Clean,
        Extension::Dirty,
    ];
    assert_eq!(EXTENSIONS, 0x6600);
    for fs in 0..4 {
        for vs in 0..4 {
            for enabled in [false, true] {
                // Unrelated SPIE and SUM fields must not affect these predicates.
                let bits = (fs << 13) | (vs << 9) | usize::from(enabled) * 2 | (1 << 5) | (1 << 18);
                let status = Status::from_bits(bits);
                assert_eq!(status.masked(), !enabled);
                assert_eq!(status.floating(), states[fs]);
                assert_eq!(status.vector(), states[vs]);
                assert_eq!(status.extensions_off(), fs == 0 && vs == 0);
            }
        }
    }
}
