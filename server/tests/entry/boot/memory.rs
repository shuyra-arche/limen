use super::{
    DeviceTree, DtbMemory, FirmwareRegion, Loader,
    devicetree::memory::{Error, Physical, Source, describe as describe_into, read},
};

fn describe(blob: &[u8]) -> Result<DtbMemory, Error> {
    let mut memory = None;
    let result = describe_into(blob, &mut memory);
    assert_eq!(memory.is_some(), result.is_ok());
    result.map(|()| memory.unwrap())
}

#[derive(Default)]
struct Tree {
    structure: Vec<u8>,
    strings: Vec<u8>,
    reservations: Vec<(u64, u64)>,
}

impl Tree {
    fn token(&mut self, word: u32) {
        self.structure.extend(word.to_be_bytes());
    }

    fn bytes(&mut self, bytes: &[u8]) {
        self.structure.extend(bytes);
        self.structure
            .resize(self.structure.len().next_multiple_of(4), 0);
    }

    fn node(&mut self, name: &str) {
        self.token(1);
        self.bytes(&[name.as_bytes(), &[0]].concat());
    }

    fn property(&mut self, name: &str, data: &[u8]) {
        self.token(3);
        self.token(u32::try_from(data.len()).unwrap());
        self.token(u32::try_from(self.strings.len()).unwrap());
        self.strings.extend(name.as_bytes());
        self.strings.push(0);
        self.bytes(data);
    }

    fn cells(&mut self, address: u32, size: u32) {
        self.property("#address-cells", &address.to_be_bytes());
        self.property("#size-cells", &size.to_be_bytes());
    }

    fn memory(&mut self, data: &[u8]) {
        self.node("memory@80000000");
        self.property("device_type", b"memory\0");
        self.property("reg", data);
        self.token(2);
    }

    fn reserved(&mut self) {
        self.node("reserved-memory");
        self.cells(2, 2);
        self.property("ranges", b"");
    }

    fn finish(mut self, terminated_reservations: bool) -> Vec<u8> {
        self.token(2);
        self.token(9);
        if terminated_reservations {
            self.reservations.push((0, 0));
        }
        let reserve_start = 40;
        let structure_start = reserve_start + self.reservations.len() * 16;
        let strings_start = structure_start + self.structure.len();
        let mut blob = Vec::new();
        for word in [
            0xd00d_feed,
            strings_start + self.strings.len(),
            structure_start,
            strings_start,
            reserve_start,
            17,
            16,
            0,
            self.strings.len(),
            self.structure.len(),
        ] {
            blob.extend(u32::try_from(word).unwrap().to_be_bytes());
        }
        for (start, size) in self.reservations {
            blob.extend(start.to_be_bytes());
            blob.extend(size.to_be_bytes());
        }
        blob.extend(self.structure);
        blob.extend(self.strings);
        blob
    }
}

fn pair(start: u64, size: u64) -> [u8; 16] {
    let mut bytes = [0; 16];
    bytes[..8].copy_from_slice(&start.to_be_bytes());
    bytes[8..].copy_from_slice(&size.to_be_bytes());
    bytes
}

fn root() -> Tree {
    let mut tree = Tree::default();
    tree.node("");
    tree.cells(2, 2);
    tree
}

#[test]
fn uefi_handoff_exposes_only_whole_unreserved_pages_corroborated_by_both_sources() {
    let mut tree = root();
    tree.memory(&pair(0x1801, 0xd000));
    tree.reservations.push((0x3400, 0x2300));
    tree.reserved();
    tree.node("firmware@5200");
    tree.property("reg", &pair(0x5200, 0x1100));
    tree.property("no-map", b"");
    tree.token(2);
    tree.token(2);

    #[repr(align(8))]
    struct Bytes<const N: usize>([u8; N]);
    let tree = tree.finish(true);
    let mut storage = Bytes([0; 1024]);
    let bytes = &mut storage.0[..tree.len()];
    bytes.copy_from_slice(&tree);

    let mut descriptors = Bytes([0; 5 * 48]);
    for (entry, (kind, start, pages, attributes)) in
        descriptors.0.as_chunks_mut::<48>().0.iter_mut().zip([
            (7, 0xc000, 3, 0x0f),
            (2, bytes.as_ptr().addr() as u64 & !4095, 2, 0),
            (7, 0x1000, 8, 8),
            (7, 0xa000, 1, 1 << 63),
            (11, 0xb000, 1, 1),
        ])
    {
        super::describe(entry, kind, start, pages, attributes);
    }
    let loader = Loader {
        descriptors: &descriptors.0,
        stride: 48,
        version: 1,
        descriptor_limit: 5,
        device_tree: Some(DeviceTree::Memory(bytes)),
    };
    let boot = loader.admit(7).unwrap();
    assert_eq!(boot.firmware_map().unwrap().regions().len(), 5);
    assert_eq!(boot.dtb_memory().unwrap().reservations().len(), 2);
    let expected = [
        (0xc000..0xe000, 0x0f),
        (0x2000..0x3000, 8),
        (0x7000..0x9000, 8),
    ];
    assert!(
        boot.conventional_ram()
            .unwrap()
            .eq(expected.map(|(physical, attributes)| {
                FirmwareRegion {
                    kind: 7,
                    physical,
                    attributes,
                }
            }))
    );
}

#[test]
fn reports_multiple_ram_ranges_and_preserves_both_reservation_sources() {
    let mut tree = root();
    let ranges = [pair(0x8000_0000, 0x80_0000), pair(0x1_0000_0000, 0x40_0000)].concat();
    tree.memory(&ranges);
    tree.reservations.push((0x8000_0000, 0x3000));
    tree.reserved();
    tree.node("firmware@80001000");
    tree.property("reg", &pair(0x8000_1000, 0x1000));
    tree.property("no-map", b"");
    tree.node("metadata");
    tree.property("compatible", b"limen,reservation-fixture\0");
    tree.token(2);
    tree.token(2);
    tree.node("pool@100001000");
    tree.property("reg", &pair(0x1_0000_1000, 0x1000));
    tree.property("reusable", b"");
    tree.token(2);
    tree.node("unused");
    tree.property("status", b"disabled\0");
    tree.property("size", &0x4000u64.to_be_bytes());
    tree.token(2);
    tree.token(2);
    let blob = tree.finish(true);
    let map = describe(&blob).unwrap();
    assert_eq!(
        map.ram(),
        &[
            Physical {
                start: 0x8000_0000,
                end: 0x8080_0000
            },
            Physical {
                start: 0x1_0000_0000,
                end: 0x1_0040_0000
            },
        ]
    );
    let reservations = map.reservations();
    assert_eq!(reservations.len(), 3);
    assert_eq!(reservations[0].source, Source::ReservationBlock);
    assert_eq!(
        reservations[0].physical,
        Physical {
            start: 0x8000_0000,
            end: 0x8000_3000
        }
    );
    assert_eq!(
        reservations[1].source,
        Source::ReservedMemory {
            no_map: true,
            reusable: false
        }
    );
    assert_eq!(
        reservations[1].physical,
        Physical {
            start: 0x8000_1000,
            end: 0x8000_2000
        }
    );
    assert_eq!(
        reservations[2].source,
        Source::ReservedMemory {
            no_map: false,
            reusable: true
        }
    );
    assert_eq!(
        reservations[2].physical,
        Physical {
            start: 0x1_0000_1000,
            end: 0x1_0000_2000
        }
    );
}

#[test]
fn uses_the_parent_cell_widths_and_skips_unrelated_nested_properties() {
    let mut tree = Tree::default();
    tree.node("");
    tree.cells(1, 1);
    tree.memory(&[0x80, 0, 0, 0, 0, 0x10, 0, 0]);
    tree.node("bus");
    tree.cells(3, 0);
    tree.node("device@0");
    tree.property("reg", &[0; 12]);
    tree.token(2);
    tree.token(2);
    let blob = tree.finish(true);
    assert_eq!(
        describe(&blob).unwrap().ram(),
        &[Physical {
            start: 0x8000_0000,
            end: 0x8010_0000
        }]
    );
}

#[test]
fn rejects_truncation_bad_cell_widths_and_invalid_reg_tuples() {
    let mut tree = root();
    tree.memory(&pair(0x8000_0000, 0x1000));
    let blob = tree.finish(true);
    for length in 0..blob.len() {
        assert!(
            describe(&blob[..length]).is_err(),
            "accepted {length} bytes"
        );
    }
    let mut tree = root();
    tree.memory(&[0; 12]);
    assert_eq!(describe(&tree.finish(true)).unwrap_err(), Error::Format);
    let mut tree = Tree::default();
    tree.node("");
    tree.cells(3, 2);
    tree.memory(&pair(0x8000_0000, 0x1000));
    assert_eq!(
        describe(&tree.finish(true)).unwrap_err(),
        Error::Unsupported
    );
}

#[test]
fn bounds_the_retained_extent_separately_from_the_parsed_blocks() {
    let mut tree = root();
    tree.memory(&pair(0x4000_0000, 0x1000_0000));
    let mut blob = tree.finish(true);
    let aavmf_extent = 1024 * 1024 + 4096;
    blob.resize(aavmf_extent, 0);
    blob[4..8].copy_from_slice(&(aavmf_extent as u32).to_be_bytes());
    assert_eq!(
        describe(&blob).unwrap().ram(),
        &[Physical {
            start: 0x4000_0000,
            end: 0x5000_0000
        }]
    );

    // Run beyond the block budget while the retained extent still fits.
    let strings = 256 * 1024u32;
    blob[32..36].copy_from_slice(&strings.to_be_bytes());
    assert_eq!(describe(&blob).unwrap_err(), Error::Capacity);

    let oversized = 2 * 1024 * 1024 + 1;
    blob.resize(oversized, 0);
    blob[4..8].copy_from_slice(&(oversized as u32).to_be_bytes());
    assert_eq!(describe(&blob).unwrap_err(), Error::Capacity);
}

#[test]
fn rejects_invalid_ranges_and_capacity_breaches_without_a_description() {
    for tuples in [
        pair(u64::MAX - 4095, 4096).to_vec(),
        pair(0x8000_0000, 0).to_vec(),
        [pair(0x8000_0000, 0x2000), pair(0x8000_1000, 0x1000)].concat(),
    ] {
        let mut tree = root();
        tree.memory(&tuples);
        assert_eq!(describe(&tree.finish(true)).unwrap_err(), Error::Range);
    }
    let mut tree = root();
    let banks: Vec<_> = (0..17)
        .flat_map(|index| pair(0x8000_0000 + index * 0x1000, 0x1000))
        .collect();
    tree.memory(&banks);
    assert_eq!(describe(&tree.finish(true)).unwrap_err(), Error::Capacity);
    let mut tree = root();
    tree.memory(&pair(0x8000_0000, 0x1000));
    tree.reservations
        .extend((0..65).map(|index| (0x8000_0000 + index * 0x1000, 0x1000)));
    assert_eq!(describe(&tree.finish(true)).unwrap_err(), Error::Capacity);
}

#[test]
fn rejects_unterminated_reservation_maps_and_unresolved_dynamic_reservations() {
    let mut tree = root();
    tree.memory(&pair(0x8000_0000, 0x1000));
    tree.reservations.push((0x8000_0000, 0x1000));
    assert_eq!(describe(&tree.finish(false)).unwrap_err(), Error::Format);

    let mut tree = root();
    tree.memory(&pair(0x8000_0000, 0x1000));
    tree.reserved();
    tree.node("pool");
    tree.property("size", &4096u64.to_be_bytes());
    tree.token(2);
    tree.token(2);
    assert_eq!(
        describe(&tree.finish(true)).unwrap_err(),
        Error::Unsupported
    );
}

#[test]
fn rejects_corrupt_structure_and_excess_nesting() {
    let mut tree = root();
    tree.memory(&pair(0x8000_0000, 0x1000));
    let mut blob = tree.finish(true);
    let strings_start = u32::from_be_bytes(blob[12..16].try_into().unwrap()) as usize;
    blob[strings_start - 4..strings_start].copy_from_slice(&0u32.to_be_bytes());
    assert_eq!(describe(&blob).unwrap_err(), Error::Format);

    let mut tree = root();
    tree.memory(&pair(0x8000_0000, 0x1000));
    for _ in 0..32 {
        tree.node("bus");
    }
    for _ in 0..32 {
        tree.token(2);
    }
    assert_eq!(describe(&tree.finish(true)).unwrap_err(), Error::Capacity);
}

#[test]
fn rejects_unsupported_reserved_memory_translation_and_conflicting_flags() {
    let mut tree = root();
    tree.memory(&pair(0x8000_0000, 0x1000));
    tree.node("reserved-memory");
    tree.cells(2, 2);
    tree.property("ranges", &[0; 24]);
    tree.token(2);
    assert_eq!(
        describe(&tree.finish(true)).unwrap_err(),
        Error::Unsupported
    );

    let mut tree = root();
    tree.memory(&pair(0x8000_0000, 0x1000));
    tree.reserved();
    tree.node("pool@80000000");
    tree.property("reg", &pair(0x8000_0000, 0x1000));
    tree.property("no-map", b"");
    tree.property("reusable", b"");
    tree.token(2);
    tree.token(2);
    assert_eq!(describe(&tree.finish(true)).unwrap_err(), Error::Format);
}

#[test]
fn checks_the_trusted_readable_window_before_reading_the_blob() {
    #[repr(align(8))]
    struct Storage([u8; 4096]);

    let mut tree = root();
    tree.memory(&pair(0x8000_0000, 0x1000));
    let blob = tree.finish(true);
    let mut storage = Storage([0; 4096]);
    storage.0[..blob.len()].copy_from_slice(&blob);
    let input = storage.0.as_ptr().cast();
    let start = storage.0.as_ptr().addr();
    let end = start + blob.len();
    // The backing fixture is truly readable and immutable throughout these calls.
    assert_eq!(
        unsafe { read(input, start..start + 39, 0..0) }.unwrap_err(),
        Error::Window
    );
    assert_eq!(
        unsafe { read(input, start..end - 1, 0..0) }.unwrap_err(),
        Error::Window
    );
    assert_eq!(
        unsafe { read(input, start..end, end - 1..end + 1) }.unwrap_err(),
        Error::Format
    );
    let (extent, memory) = unsafe { read(input, start..end, 0..0) }.unwrap();
    assert_eq!(extent, start..end);
    assert_eq!(memory.ram().len(), 1);
}
