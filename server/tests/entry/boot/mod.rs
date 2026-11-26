use super::{Boot, Retirement, physical, regions};
use crate::metal::platform::firmware::{
    devicetree::{self, memory::DtbMemory},
    facts::Descriptions,
    map::{FirmwareMap, FirmwareRegion, MapError},
    uefi::{AcpiInput, DeviceTree, Loader},
};
use limen_treaty::model::{PhysicalAddress, PhysicalRange};

mod exclusions;
mod memory;
pub(crate) mod organization;

pub(crate) fn range(start: u64, end: u64) -> PhysicalRange {
    PhysicalRange::new(PhysicalAddress::new(start), PhysicalAddress::new(end)).unwrap()
}

pub(crate) fn image() -> PhysicalRange {
    range(0x1000, 0x2000)
}

#[test]
fn admits_only_the_reserved_dtb_extent_from_a_strided_final_map() {
    #[repr(align(8))]
    struct Bytes<const N: usize>([u8; N]);
    let mut dtb = Bytes([0; 96]);
    let words = [0xd00d_feed_u32, 96, 56, 72, 40, 17, 16, 0, 0, 16];
    for (dst, word) in dtb.0.as_chunks_mut::<4>().0.iter_mut().zip(words) {
        dst.copy_from_slice(&word.to_be_bytes());
    }
    for (offset, token) in [(56, 1u32), (64, 2), (68, 9)] {
        dtb.0[offset..offset + 4].copy_from_slice(&token.to_be_bytes());
    }
    let mut descriptors = Bytes([0u8; 96]);
    // A real reserved descriptor, not a zero-length placeholder.
    descriptors.0[24..32].copy_from_slice(&1u64.to_le_bytes());
    let d = &mut descriptors.0[48..];
    d[..4].copy_from_slice(&2u32.to_le_bytes());
    d[8..16].copy_from_slice(&((dtb.0.as_ptr().addr() as u64) & !4095).to_le_bytes());
    d[24..32].copy_from_slice(&2u64.to_le_bytes());
    let admit = |descriptors: &[u8], stride, version, tree: Option<&[u8]>| {
        let loader = Loader {
            image: image(),
            descriptors,
            stride,
            version,
            descriptor_limit: 2,
            device_tree: tree.map(DeviceTree::Description),
            acpi: None,
        };
        let mut descriptions = Descriptions::new();
        admit_loader(&loader, &mut descriptions).map(|b| {
            assert_eq!(b.hart(), 7);
            assert_eq!(b.firmware_map().unwrap().stride(), stride);
            assert!(b.dtb_memory().is_none());
            assert_eq!(b.device_tree_description().map(|view| view.bytes()), tree);
            assert!(b.conventional_ram().is_none());
            b.device_tree()
        })
    };
    assert_eq!(
        admit(&descriptors.0, 48, 1, Some(&dtb.0)),
        Some(Some(dtb.0.as_ptr().addr()..dtb.0.as_ptr().addr() + 96))
    );
    assert_eq!(admit(&descriptors.0, 48, 1, None), Some(None));
    let loader = Loader {
        image: image(),
        descriptors: &descriptors.0,
        stride: 48,
        version: 1,
        descriptor_limit: 2,
        device_tree: Some(DeviceTree::Memory(&dtb.0)),
        acpi: None,
    };
    assert!(admit_loader(&loader, &mut Descriptions::new()).is_none());
    assert!(admit(&descriptors.0, 48, 1, Some(&dtb.0[..40])).is_none());
    assert!(admit(&descriptors.0, 0, 1, None).is_none());
    assert!(admit(&descriptors.0, 48, 2, None).is_none());
    descriptors.0[48] = 4;
    assert!(admit(&descriptors.0, 48, 1, Some(&dtb.0)).is_none());
}

#[repr(align(8))]
struct DescriptorBytes([u8; 96]);

pub(crate) fn describe(entry: &mut [u8], kind: u32, base: u64, pages: u64, attributes: u64) {
    entry[..4].copy_from_slice(&kind.to_le_bytes());
    entry[8..16].copy_from_slice(&base.to_le_bytes());
    entry[24..32].copy_from_slice(&pages.to_le_bytes());
    entry[32..40].copy_from_slice(&attributes.to_le_bytes());
}

#[test]
fn firmware_map_preserves_unsorted_adjacent_regions_and_raw_attributes() {
    let mut bytes = DescriptorBytes([0xff; 96]);
    describe(&mut bytes.0[..48], 7, 0x3000, 2, 8);
    describe(&mut bytes.0[48..], 0, 0x1000, 2, 0x8000_0000_0000_0000);
    let map = FirmwareMap::new(&bytes.0, 48, 1, 2).unwrap();
    assert_eq!(map.descriptors(), &bytes.0);
    assert_eq!(map.stride(), 48);
    assert_eq!(map.version(), 1);
    let mut regions = map.regions();
    assert_eq!(regions.len(), 2);
    let first = regions.next().unwrap();
    assert_eq!(first.physical, range(0x3000, 0x5000));
    assert_eq!(first.kind, 7);
    assert_eq!(first.attributes, 8);
    let second = regions.next().unwrap();
    assert_eq!(second.physical, range(0x1000, 0x3000));
    assert_eq!(second.kind, 0);
    assert_eq!(second.attributes, 0x8000_0000_0000_0000);
    assert!(regions.next().is_none());
}

#[test]
fn firmware_map_rejects_bad_geometry_overlaps_and_capacity_before_handoff() {
    let mut bytes = DescriptorBytes([0; 96]);
    describe(&mut bytes.0[..48], 7, 0x1000, 1, 0);
    describe(&mut bytes.0[48..], 2, 0x2000, 1, 0);
    assert_eq!(
        FirmwareMap::new(&bytes.0, 48, 1, 1).err(),
        Some(MapError::Capacity)
    );
    assert_eq!(
        FirmwareMap::new(&bytes.0, 40, 1, 2).err(),
        Some(MapError::Format)
    );
    assert_eq!(
        FirmwareMap::new(&bytes.0, 48, 2, 2).err(),
        Some(MapError::Format)
    );
    assert_eq!(
        FirmwareMap::new(&bytes.0[1..49], 48, 1, 2).err(),
        Some(MapError::Format)
    );
    for (base, pages, error) in [
        (0x1000, 1, MapError::Overlap),
        (0x1001, 1, MapError::Range),
        (0x2000, 0, MapError::Range),
        (0x2000, u64::MAX, MapError::Range),
        (u64::MAX - 4095, 1, MapError::Range),
    ] {
        describe(&mut bytes.0[48..], 2, base, pages, 0);
        assert_eq!(FirmwareMap::new(&bytes.0, 48, 1, 2).err(), Some(error));
        let loader = Loader {
            image: image(),
            descriptors: &bytes.0,
            stride: 48,
            version: 1,
            descriptor_limit: 2,
            device_tree: None,
            acpi: None,
        };
        assert!(admit_loader(&loader, &mut Descriptions::new()).is_none());
    }
}

#[test]
fn checks_the_firmware_map_before_requesting_acpi_bytes() {
    use core::cell::Cell;

    let root = PhysicalAddress::new(0x4ff0);
    for (kind, pages, expected) in [
        (7, 2, None),
        (9, 1, None),
        (9, 2, Some(range(0x4ff0, 0x5004))),
    ] {
        let requested = Cell::new(None);
        let deny = |physical| {
            requested.set(Some(physical));
            None
        };
        let mut descriptors = DescriptorBytes([0; 96]);
        describe(&mut descriptors.0[..48], kind, 0x4000, pages, 0);
        let loader = Loader {
            image: image(),
            descriptors: &descriptors.0[..48],
            stride: 48,
            version: 1,
            descriptor_limit: 1,
            device_tree: None,
            acpi: Some(AcpiInput {
                rsdp: root,
                read: &deny,
            }),
        };
        assert!(admit_loader(&loader, &mut Descriptions::new()).is_none());
        assert_eq!(requested.take(), expected);
    }
}

fn admit_loader<'a>(
    loader: &Loader<'a>,
    descriptions: &'a mut Descriptions<'a>,
) -> Option<Boot<'a>> {
    let facts = loader.admit(descriptions)?;
    Some(Boot::new(7, facts.image, facts.firmware_map, descriptions))
}
