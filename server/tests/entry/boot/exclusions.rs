use super::handoff::Boot;
use super::physical::{Condition, Occupant, Retirement};
use super::{FirmwareMap, PhysicalAddress, PhysicalRange, describe, image, range};

const CONDITIONS: [Condition; 4] = [
    Condition::RuntimeStack,
    Condition::ReadersGone,
    Condition::TranslationRetired,
    Condition::DevicesIsolated,
];

#[test]
fn every_condition_must_cover_the_requested_physical_scope() {
    #[repr(align(8))]
    struct Descriptors([u8; 40]);
    let mut bytes = Descriptors([0; 40]);
    describe(&mut bytes.0, 4, 0x4000, 3, 0);
    let map = FirmwareMap::new(&bytes.0, 40, 1, 1).unwrap();
    let boot = Boot::from_uefi(0, image(), Some(0x6001..0x6010), map, None);
    let early = boot.exclusions_for(range(0x1100, 0x1200), range(0x1300, 0x1400));
    let page = range(0x4000, 0x5000);
    assert!(early.retains(page, None));
    for missing in CONDITIONS {
        let mut retirement = Retirement::new(page);
        for condition in CONDITIONS
            .into_iter()
            .rev()
            .filter(|value| *value != missing)
        {
            // The fixture models a completed condition for the exact scope.
            unsafe { retirement.record(condition) };
        }
        assert!(early.retains(page, Some(&retirement)));
        unsafe { retirement.record(missing) };
        assert!(!early.retains(page, Some(&retirement)));
        assert!(early.retains(range(0x4000, 0x5001), Some(&retirement)));
        assert!(early.retains(range(0x5000, 0x6000), Some(&retirement)));
    }
    let start = PhysicalAddress::new(0x4000);
    let end = PhysicalAddress::new(0x5000);
    assert!(PhysicalRange::new(start, start).is_none());
    assert!(PhysicalRange::new(end, start).is_none());
}

#[test]
fn fixed_claims_outlive_retirement_evidence_and_preserve_partial_pages() {
    #[repr(align(8))]
    struct Descriptors([u8; 120]);
    let mut bytes = Descriptors([0; 120]);
    describe(&mut bytes.0[..40], 2, 0x1000, 1, 0);
    describe(&mut bytes.0[40..80], 7, 0x3000, 1, 1 << 63);
    // Old access can end before ACPI has been enabled.
    describe(&mut bytes.0[80..], 9, 0x5000, 1, 0);
    let map = FirmwareMap::new(&bytes.0, 40, 1, 3).unwrap();
    let boot = Boot::from_uefi(0, image(), Some(0x6001..0x6010), map, None);
    let early = boot.exclusions_for(range(0x1100, 0x1200), range(0x1300, 0x1400));
    assert!(
        early
            .entries()
            .any(|entry| entry.occupant == Occupant::FirmwareMap && entry.fixed())
    );
    assert!(
        early
            .entries()
            .any(|entry| entry.occupant == Occupant::MajorStack && entry.fixed())
    );
    let mut retirement = Retirement::new(range(0x1000, 0x7000));
    for condition in CONDITIONS {
        unsafe { retirement.record(condition) };
    }
    for retained in [
        range(0x1000, 0x2000),
        range(0x3000, 0x4000),
        range(0x5000, 0x6000),
        range(0x6000, 0x7000),
    ] {
        assert!(early.retains(retained, Some(&retirement)));
    }
    assert!(!early.retains(range(0x2000, 0x3000), Some(&retirement)));
}
