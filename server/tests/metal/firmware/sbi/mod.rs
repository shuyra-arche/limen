#[path = "../../../../src/metal/firmware/sbi/probe.rs"]
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

#[test]
fn discovers_hart_timer_and_ipi_services_independently() {
    for hart in [false, true] {
        for timer in [false, true] {
            for ipi in [false, true] {
                let found =
                    probe::SbiServices::probe(|function, extension| match (function, extension) {
                        (0, 0) => (0, 1 << 24),
                        (3, 0x0048_534d) => (0, usize::from(hart)),
                        (3, 0x5449_4d45) => (0, usize::from(timer)),
                        (3, 0x0073_5049) if ipi => (0, 3),
                        (3, 0x0073_5049) => (-2, usize::MAX),
                        _ => panic!("unexpected SBI base call"),
                    });
                assert_eq!(found.hart_state_management(), hart);
                assert_eq!(found.timer(), timer);
                assert_eq!(found.inter_processor_interrupt(), ipi);
            }
        }
    }
}

#[test]
fn rejects_a_base_version_with_reserved_bits_before_discovery() {
    let found = probe::SbiServices::probe(|function, extension| {
        assert_eq!((function, extension), (0, 0));
        (0, 1 << 31)
    });
    assert!(!found.hart_state_management());
    assert!(!found.timer());
    assert!(!found.inter_processor_interrupt());
}
