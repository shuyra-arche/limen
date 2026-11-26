use super::physical::Condition;
use super::regions::{Region, RegionError, RegionSource, Regions};
use super::{
    Boot, FirmwareMap, PhysicalAddress, PhysicalRange, Retirement, describe, image, range,
};
use crate::access::{Error as AccessError, MappedRange};
use core::{alloc::Layout, mem::MaybeUninit};
use limen_treaty::model::{Alignment, ByteLength};

const GRANULE: ByteLength = ByteLength::new(4096);

pub(super) fn retired(scope: PhysicalRange) -> Retirement {
    let mut evidence = Retirement::new(scope);
    // Host fixtures model complete evidence for the given range.
    for condition in [
        Condition::DevicesIsolated,
        Condition::TranslationRetired,
        Condition::ReadersGone,
        Condition::RuntimeStack,
    ] {
        unsafe { evidence.record(condition) };
    }
    evidence
}

#[repr(align(8))]
struct Descriptors<const N: usize>([u8; N]);

#[test]
fn physical_ranges_check_length_endpoints_and_upper_alignment() {
    let zero = PhysicalAddress::new(0);
    let last = PhysicalAddress::new(u64::MAX);
    let one = ByteLength::new(1);
    let maximum = ByteLength::new(u64::MAX);
    assert_eq!(zero.range(ByteLength::new(0)), None);
    assert_eq!(last.range(one), None);
    assert_eq!(last.distance(zero), Some(maximum));
    assert_eq!(zero.distance(last), None);

    let whole = zero.range(maximum).unwrap();
    assert_eq!(whole, range(0, u64::MAX));
    assert!(whole.contains(zero));
    assert!(!whole.contains(last));

    let alignment = Alignment::new(4096).unwrap();
    let upper = PhysicalAddress::new(u64::MAX - 4095);
    assert!(upper.is_aligned(alignment));
    assert!(!last.is_aligned(alignment));
    assert_eq!(last.align_down(alignment), upper);
    assert_eq!(upper.align_up(alignment), Some(upper));
    assert_eq!(last.align_up(alignment), None);
}

#[test]
fn sorts_sources_clips_the_evidence_and_excludes_partial_reserved_pages() {
    let mut storage = Descriptors([0; 120]);
    describe(&mut storage.0[..40], 7, 0x8000, 3, 0x21);
    describe(&mut storage.0[40..80], 7, 0x2000, 4, 8);
    describe(&mut storage.0[80..], 7, 0x6000, 2, 1 << 63);
    let map = FirmwareMap::new(&storage.0, 40, 1, 3).unwrap();
    let boot = Boot::from_uefi(0, image(), Some(0x4301..0x4500), map, None);
    let exclusions = boot.exclusions_for(image(), image());
    let evidence = retired(range(0x2fff, 0xa001));
    let expected = [
        Region {
            physical: range(0x3000, 0x4000),
            source: RegionSource::Uefi { attributes: 8 },
        },
        Region {
            physical: range(0x5000, 0x6000),
            source: RegionSource::Uefi { attributes: 8 },
        },
        Region {
            physical: range(0x8000, 0xa000),
            source: RegionSource::Uefi { attributes: 0x21 },
        },
    ];
    let mut regions = [None; 3];
    let ready =
        Regions::organize(&boot, &exclusions, GRANULE, Some(&evidence), &mut regions).unwrap();
    assert!(ready.entries().eq(expected));
    let result = Regions::organize(
        &boot,
        &exclusions,
        GRANULE,
        Some(&evidence),
        &mut regions[..2],
    );
    assert_eq!(result.err(), Some(RegionError::Capacity));
    assert_eq!(&regions[..2], &[None, None]);
    for invalid in [0, 3] {
        assert_eq!(
            Regions::organize(
                &boot,
                &exclusions,
                ByteLength::new(invalid),
                Some(&evidence),
                &mut regions,
            )
            .err(),
            Some(RegionError::Granule)
        );
    }
    let partial = Retirement::new(range(0x2000, 0xa000));
    let held =
        Regions::organize(&boot, &exclusions, GRANULE, Some(&partial), &mut regions).unwrap();
    assert_eq!(held.entries().count(), 0);
}

#[test]
fn a_root_consumes_a_mapped_grant_and_removes_its_whole_physical_page() {
    let mut descriptors = Descriptors([0; 40]);
    describe(&mut descriptors.0, 7, 0x2000, 7, 8);
    let map = FirmwareMap::new(&descriptors.0, 40, 1, 1).unwrap();
    let boot = Boot::from_uefi(0, image(), None, map, None);
    let exclusions = boot.exclusions_for(image(), image());
    let evidence = retired(range(0x2000, 0x9000));
    let layout = Layout::from_size_align(17, 64).unwrap();
    #[repr(align(8192))]
    struct Backing([MaybeUninit<u8>; 8192]);
    let mut backing = Backing([MaybeUninit::uninit(); 8192]);
    let mut mapping =
        Some(unsafe { MappedRange::new(range(0x3001, 0x5000), &mut backing.0[1..]) }.unwrap());

    let mut small = [None; 1];
    let mut geometry =
        Regions::organize(&boot, &exclusions, GRANULE, Some(&evidence), &mut small).unwrap();
    assert!(geometry.reserve_root(&mut None, layout).unwrap().is_none());
    assert_eq!(
        geometry.reserve_root(&mut mapping, layout).err(),
        Some(RegionError::Capacity)
    );
    assert!(mapping.is_some());
    assert!(
        geometry
            .entries()
            .map(|r| r.physical)
            .eq([range(0x2000, 0x9000)])
    );

    let mut space = [None; 2];
    let mut held = Regions::organize(&boot, &exclusions, GRANULE, None, &mut space).unwrap();
    assert!(held.reserve_root(&mut mapping, layout).unwrap().is_none());
    assert!(mapping.is_some());

    let mut geometry =
        Regions::organize(&boot, &exclusions, GRANULE, Some(&evidence), &mut space).unwrap();
    let mismatched = Layout::from_size_align(17, 8192).unwrap();
    assert_eq!(
        geometry.reserve_root(&mut mapping, mismatched).err(),
        Some(RegionError::Layout)
    );
    let too_large = Layout::from_size_align(8193, 64).unwrap();
    assert_eq!(
        geometry.reserve_root(&mut mapping, too_large).err(),
        Some(RegionError::Layout)
    );
    assert!(mapping.is_some());
    let mut root = geometry
        .reserve_root(&mut mapping, layout)
        .unwrap()
        .unwrap();
    assert!(mapping.is_none());
    assert_eq!(root.physical(), range(0x3040, 0x3051));
    assert_eq!(root.reserved(), range(0x3000, 0x4000));
    assert_eq!(root.source(), RegionSource::Uefi { attributes: 8 });
    assert_eq!(root.bytes().len(), 17);
    root.bytes()[0].write(0xaa);
    assert!(geometry.entries().eq([
        Region {
            physical: range(0x2000, 0x3000),
            source: root.source(),
        },
        Region {
            physical: range(0x4000, 0x9000),
            source: root.source(),
        },
    ]));
}

#[test]
fn exclusive_mapping_checks_extent_and_splits_without_losing_the_original() {
    let mut bytes = [MaybeUninit::uninit(); 16];
    let address = bytes.as_mut_ptr().addr();
    for invalid in [range(0, 15), range(0, 17), range(0, u64::MAX)] {
        assert!(matches!(
            unsafe { MappedRange::new(invalid, &mut bytes) },
            Err(AccessError::Range)
        ));
    }

    let start = u64::MAX - 16;
    let physical = range(start, u64::MAX);
    // This Host fixture models the exact physical correspondence of the
    // exclusive backing. No target mapping or hardware evidence is inferred.
    let mut mapping = unsafe { MappedRange::new(physical, &mut bytes) }.unwrap();
    for offset in [0, 16, 17, u64::MAX] {
        let (mut original, error) = mapping.split(ByteLength::new(offset)).err().unwrap();
        assert_eq!(error, AccessError::Split);
        assert_eq!(original.physical(), physical);
        assert_eq!(original.length(), 16);
        assert_eq!(original.bytes().as_mut_ptr().addr(), address);
        mapping = original;
    }
    let (mut left, mut right) = mapping.split(ByteLength::new(5)).ok().unwrap();
    assert_eq!(left.physical(), range(start, start + 5));
    assert_eq!(right.physical(), range(start + 5, u64::MAX));
    assert_eq!(left.length(), 5);
    assert_eq!(right.length(), 11);
    assert_eq!(left.bytes().as_mut_ptr().addr(), address);
    assert_eq!(right.bytes().as_mut_ptr().addr(), address + 5);
    left.bytes()[4].write(0x41);
    right.bytes()[0].write(0x42);
    assert_eq!(unsafe { bytes[4].assume_init() }, 0x41);
    assert_eq!(unsafe { bytes[5].assume_init() }, 0x42);
}

#[test]
fn identity_mapping_checks_raw_geometry_before_borrowing() {
    let mut bytes = [MaybeUninit::uninit(); 16];
    let base = core::ptr::NonNull::new(bytes.as_mut_ptr()).unwrap();
    let start = base.addr().get() as u64;
    for invalid in [
        range(start + 1, start + 17),
        range(start, start + isize::MAX as u64 + 1),
    ] {
        assert!(matches!(
            unsafe { MappedRange::from_identity(invalid, base) },
            Err(AccessError::Range)
        ));
    }

    // This fixture models an identity view of its one backing array.
    let physical = range(start, start + 16);
    let mut mapping = unsafe { MappedRange::from_identity(physical, base) }.unwrap();
    assert_eq!(mapping.physical(), physical);
    assert_eq!(mapping.bytes().as_mut_ptr(), base.as_ptr());
    mapping.bytes()[15].write(0x6c);
    assert_eq!(unsafe { bytes[15].assume_init() }, 0x6c);
}
