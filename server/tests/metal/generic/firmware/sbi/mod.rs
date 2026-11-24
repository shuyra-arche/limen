#[path = "../../../../../src/metal/generic/firmware/sbi/probe.rs"]
mod probe;

#[test]
fn probes_both_extensions_independently_after_a_valid_base_version() {
    for console in [false, true] {
        for reset in [false, true] {
            let found =
                probe::Extensions::probe(|function, extension| match (function, extension) {
                    (0, 0) => (0, 2),
                    (3, 0x4442_434e) => (0, usize::from(console)),
                    (3, 0x5352_5354) if reset => (0, 7),
                    (3, 0x5352_5354) => (-2, 7),
                    _ => panic!("unexpected SBI base call"),
                });
            assert_eq!((found.console, found.reset), (console, reset));
        }
    }
}

#[test]
fn an_unavailable_base_never_probes_optional_extensions() {
    for version in [(-2, usize::MAX), (0, 1)] {
        let found = probe::Extensions::probe(|function, extension| {
            assert_eq!((function, extension), (0, 0));
            version
        });
        assert!(!found.console && !found.reset);
    }
}
