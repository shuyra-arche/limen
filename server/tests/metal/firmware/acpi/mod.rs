use kernel::metal::address::{PhysicalAddress, PhysicalRange};
use kernel::metal::firmware::acpi::{
    Acpi, Error, IoApic, Polarity, Processor, SourceOverride, Trigger,
};

const ROOT: u64 = 0x1000;
const XSDT: u64 = 0x3000;
const FIRST: u64 = 0x5000;
const SECOND: u64 = 0x7000;
const THIRD: u64 = 0x9000;

struct Firmware(Vec<(u64, Vec<u8>)>);

impl Firmware {
    fn new(tables: Vec<(u64, Vec<u8>)>) -> Self {
        let entries = tables
            .iter()
            .flat_map(|(address, _)| address.to_le_bytes())
            .collect::<Vec<_>>();
        let mut ranges = vec![(ROOT, rsdp()), (XSDT, table(*b"XSDT", 1, &entries))];
        ranges.extend(tables);
        Self(ranges)
    }

    fn read(&self, range: PhysicalRange) -> Option<&[u8]> {
        self.0.iter().find_map(|(start, bytes)| {
            let from = range.start().get().checked_sub(*start)?;
            let to = range.end().get().checked_sub(*start)?;
            bytes.get(usize::try_from(from).ok()?..usize::try_from(to).ok()?)
        })
    }

    fn replace_xsdt(&mut self, entries: &[u8]) {
        self.0[1].1 = table(*b"XSDT", 1, entries);
    }

    fn discover(&self) -> Result<Acpi<'_>, Error> {
        Acpi::new(PhysicalAddress::new(ROOT), |range| self.read(range))
    }
}

fn range(start: u64, end: u64) -> PhysicalRange {
    PhysicalRange::new(PhysicalAddress::new(start), PhysicalAddress::new(end)).unwrap()
}

fn checksum(bytes: &mut [u8], index: usize) {
    bytes[index] = 0;
    bytes[index] = bytes
        .iter()
        .copied()
        .fold(0u8, u8::wrapping_add)
        .wrapping_neg();
}

fn rsdp() -> Vec<u8> {
    let mut bytes = vec![0; 36];
    bytes[..8].copy_from_slice(b"RSD PTR ");
    bytes[15] = 2;
    bytes[20..24].copy_from_slice(&36u32.to_le_bytes());
    bytes[24..32].copy_from_slice(&XSDT.to_le_bytes());
    checksum(&mut bytes[..20], 8);
    checksum(&mut bytes, 32);
    bytes
}

fn table(signature: [u8; 4], revision: u8, body: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0; 36];
    let length = (bytes.len() + body.len()) as u32;
    bytes[..4].copy_from_slice(&signature);
    bytes[4..8].copy_from_slice(&length.to_le_bytes());
    bytes[8] = revision;
    bytes.extend_from_slice(body);
    checksum(&mut bytes, 9);
    bytes
}

fn madt(records: &[u8]) -> Vec<u8> {
    madt_with_revision(5, records)
}

fn madt_with_revision(revision: u8, records: &[u8]) -> Vec<u8> {
    let mut body = vec![0; 8];
    body[..4].copy_from_slice(&0xfee0_0000u32.to_le_bytes());
    body.extend_from_slice(records);
    table(*b"APIC", revision, &body)
}

#[test]
fn retains_source_scoped_unknown_tables_and_typed_madt_records() {
    let records = [
        &[0, 8, 6, 11, 1, 0, 0, 0][..],
        &[9, 16, 0, 0, 2, 1, 0, 0, 2, 0, 0, 0, 7, 0, 0, 0],
        &[1, 12, 26, 0, 0, 0, 0xc0, 0xfe, 24, 0, 0, 0],
        &[2, 10, 0, 8, 18, 0, 0, 0, 15, 0],
        &[5, 12, 0, 0, 0, 0, 0xe0, 0xfe, 1, 0, 0, 0],
        &[0x80, 4, 0x6a, 0x7b],
    ]
    .concat();
    let firmware = Firmware::new(vec![
        (FIRST, table(*b"SSDT", 1, &[0xaa])),
        (SECOND, madt(&records)),
        (THIRD, table(*b"SSDT", 2, &[0xbb])),
    ]);
    let acpi = firmware.discover().unwrap();
    assert_eq!(acpi.rsdp_range(), range(ROOT, ROOT + 36));
    assert_eq!(acpi.xsdt().physical(), range(XSDT, XSDT + 60));
    assert_eq!(
        acpi.tables()
            .map(|table| (table.signature(), table.physical().start().get()))
            .collect::<Vec<_>>(),
        [(*b"SSDT", FIRST), (*b"APIC", SECOND), (*b"SSDT", THIRD)],
    );
    let madt = acpi.madt().unwrap();
    assert_eq!(madt.table().physical().start().get(), SECOND);
    assert_eq!(
        madt.local_controller_address(),
        Some(PhysicalAddress::new(0x1_fee0_0000))
    );
    let processors = madt.processors().collect::<Vec<_>>();
    assert_eq!(
        processors[0],
        Processor::Apic {
            id: 11,
            uid: 6,
            flags: 1
        }
    );
    assert_eq!(
        processors[1],
        Processor::X2Apic {
            id: 258,
            uid: 7,
            flags: 2
        }
    );
    assert!(processors[0].enabled());
    assert!(!processors[1].enabled());
    assert!(processors[1].online_capable());
    assert_eq!(
        madt.io_apics().collect::<Vec<_>>(),
        [IoApic {
            id: 26,
            address: PhysicalAddress::new(0xfec0_0000),
            gsi_base: 24
        }],
    );
    assert_eq!(
        madt.source_overrides().collect::<Vec<_>>(),
        [SourceOverride {
            isa_irq: 8,
            gsi: 18,
            polarity: Polarity::ActiveLow,
            trigger: Trigger::Level
        }],
    );
    let unknown = madt.records().last().unwrap();
    assert_eq!(unknown.offset(), 102);
    assert_eq!(unknown.kind(), 0x80);
    assert_eq!(unknown.bytes(), [0x80, 4, 0x6a, 0x7b]);
}

#[test]
fn every_child_needs_independent_prefix_and_whole_table_reads() {
    let firmware = Firmware::new(vec![(FIRST, table(*b"SSDT", 1, &[0xa5]))]);
    let mut reads = Vec::new();
    let result = Acpi::new(PhysicalAddress::new(ROOT), |range| {
        reads.push(range);
        if range.start().get() == FIRST && range.end().get() == FIRST + 37 {
            return None;
        }
        firmware.read(range)
    });
    assert_eq!(
        result.unwrap_err(),
        Error::Unreadable(range(FIRST, FIRST + 37))
    );
    assert_eq!(
        reads,
        [
            range(ROOT, ROOT + 20),
            range(ROOT, ROOT + 36),
            range(XSDT, XSDT + 36),
            range(XSDT, XSDT + 44),
            range(FIRST, FIRST + 36),
            range(FIRST, FIRST + 37),
        ],
    );
    let result = Acpi::new(PhysicalAddress::new(ROOT), |range| {
        let bytes = firmware.read(range)?;
        if range.start().get() == FIRST {
            return bytes.get(..20);
        }
        Some(bytes)
    });
    assert_eq!(
        result.unwrap_err(),
        Error::Unreadable(range(FIRST, FIRST + 36))
    );
}

#[test]
fn rejects_bad_checksums_and_inconsistent_reader_responses() {
    let mut firmware = Firmware::new(vec![(FIRST, table(*b"SSDT", 1, &[0xa5]))]);
    for (entry, index) in [(0, 8), (0, 32), (1, 9), (2, 36)] {
        firmware.0[entry].1[index] ^= 0x80;
        assert_eq!(firmware.discover().unwrap_err(), Error::InvalidChecksum);
        firmware.0[entry].1[index] ^= 0x80;
    }
    let changed = table(*b"ABCD", 1, &[0xa5]);
    let result = Acpi::new(PhysicalAddress::new(ROOT), |range| {
        if range.start().get() == FIRST && range.end().get() == FIRST + 37 {
            return Some(&changed);
        }
        firmware.read(range)
    });
    assert_eq!(result.unwrap_err(), Error::InconsistentRead);
}

#[test]
fn rejects_malformed_signatures_and_retains_unknown_printable_ones() {
    for invalid in [[0xff, b'S', b'D', b'T'], [0, b'S', b'D', b'T']] {
        let firmware = Firmware::new(vec![(FIRST, table(invalid, 1, &[]))]);
        assert_eq!(firmware.discover().unwrap_err(), Error::InvalidSignature);
    }
    let firmware = Firmware::new(vec![(FIRST, table(*b"ZZZ!", 1, &[]))]);
    let acpi = firmware.discover().unwrap();
    assert_eq!(acpi.tables().next().unwrap().signature(), *b"ZZZ!");
}

#[test]
fn distinguishes_unsupported_root_revision_missing_madt_and_invalid_layout() {
    let mut firmware = Firmware::new(Vec::new());
    assert!(firmware.discover().unwrap().madt().is_none());
    firmware.0[0].1[15] = 0;
    checksum(&mut firmware.0[0].1[..20], 8);
    firmware.0[0].1.truncate(20);
    assert_eq!(firmware.discover().unwrap_err(), Error::UnsupportedRevision);

    let mut firmware = Firmware::new(Vec::new());
    firmware.replace_xsdt(&[0, 1]);
    assert_eq!(firmware.discover().unwrap_err(), Error::InvalidLength);
    firmware.replace_xsdt(&[0; 25 * 8]);
    assert_eq!(firmware.discover().unwrap_err(), Error::Capacity);
}

#[test]
fn rejects_invalid_physical_extents_and_overlapping_entries() {
    let mut firmware = Firmware::new(Vec::new());
    firmware.replace_xsdt(&u64::MAX.to_le_bytes());
    assert_eq!(firmware.discover().unwrap_err(), Error::InvalidAddress);
    firmware.replace_xsdt(&XSDT.to_le_bytes());
    assert_eq!(firmware.discover().unwrap_err(), Error::Overlap);

    let mut firmware = Firmware::new(vec![(FIRST, table(*b"SSDT", 1, &[0; 72]))]);
    for second in [FIRST, FIRST + 64] {
        firmware.replace_xsdt(&[FIRST.to_le_bytes(), second.to_le_bytes()].concat());
        assert_eq!(firmware.discover().unwrap_err(), Error::Overlap);
    }
}

#[test]
fn rejects_second_madt_and_duplicate_usable_identities_or_routes() {
    let firmware = Firmware::new(vec![(FIRST, madt(&[])), (SECOND, madt(&[]))]);
    assert_eq!(firmware.discover().unwrap_err(), Error::DuplicateTable);
    let apic = [0, 8, 6, 11, 1, 0, 0, 0];
    let x2apic = [9, 16, 0, 0, 11, 0, 0, 0, 2, 0, 0, 0, 7, 0, 0, 0];
    let io_apic = [1, 12, 26, 0, 0, 0, 0xc0, 0xfe, 24, 0, 0, 0];
    let source = [2, 10, 0, 8, 18, 0, 0, 0, 15, 0];
    for (left, right) in [
        (&apic[..], &x2apic[..]),
        (&io_apic, &io_apic),
        (&source, &source),
    ] {
        let firmware = Firmware::new(vec![(FIRST, madt(&[left, right].concat()))]);
        assert_eq!(firmware.discover().unwrap_err(), Error::DuplicateRecord);
    }
    let absent_apic = [0, 8, 6, 11, 0, 0, 0, 0];
    for records in [[apic, absent_apic], [absent_apic, apic]] {
        let firmware = Firmware::new(vec![(FIRST, madt(&records.concat()))]);
        let tables = firmware.discover().unwrap();
        let topology = tables.madt().unwrap();
        assert_eq!(topology.processors().count(), 2);
        assert!(topology.enabled_local_apic(11));
        assert!(!topology.enabled_local_apic(12));
    }
    let firmware = Firmware::new(vec![(FIRST, madt(&absent_apic))]);
    assert!(
        !firmware
            .discover()
            .unwrap()
            .madt()
            .unwrap()
            .enabled_local_apic(11)
    );
}

#[test]
fn rejects_truncation_no_progress_invalid_binding_and_record_budget_exhaustion() {
    for bytes in [
        &[0][..],
        &[0x80, 0],
        &[0x80, 1],
        &[0x80, 3],
        &[9, 2],
        &[2, 10, 0, 8, 18, 0, 0, 0, 2, 0],
    ] {
        let firmware = Firmware::new(vec![(FIRST, madt(bytes))]);
        assert_eq!(firmware.discover().unwrap_err(), Error::InvalidRecord);
    }
    let records = [0x80, 2].repeat(257);
    let firmware = Firmware::new(vec![(FIRST, madt(&records))]);
    assert_eq!(firmware.discover().unwrap_err(), Error::Capacity);
}

#[test]
fn distinguishes_gicc_affinity_and_online_capability_from_local_apic_flags() {
    let mut gicc = [0; 82];
    gicc[0] = 11;
    gicc[1] = 82;
    gicc[8..12].copy_from_slice(&13u32.to_le_bytes());
    gicc[12..16].copy_from_slice(&8u32.to_le_bytes());
    gicc[68..76].copy_from_slice(&0xab_0012_3456u64.to_le_bytes());
    let firmware = Firmware::new(vec![(FIRST, madt_with_revision(6, &gicc))]);
    let processor = firmware
        .discover()
        .unwrap()
        .madt()
        .unwrap()
        .processors()
        .next()
        .unwrap();
    assert_eq!(
        processor,
        Processor::Gicc {
            affinity: 0xab_0012_3456,
            uid: 13,
            flags: 8
        }
    );
    assert_eq!(processor.apic_id(), None);
    assert!(processor.online_capable());

    gicc[71] = 1;
    let firmware = Firmware::new(vec![(FIRST, madt_with_revision(6, &gicc))]);
    assert_eq!(firmware.discover().unwrap_err(), Error::InvalidRecord);
}

#[test]
fn validates_gicc_flags_for_the_madt_revision() {
    for (revision, length, flags, valid) in [
        (3, 76, 0b0_0111u32, true),
        (5, 80, 0b0_0111, true),
        (5, 80, 0b0_1000, false),
        (6, 82, 0b0_1110, true),
        (6, 82, 0b1_0000, false),
        (7, 82, 0b1_0111, true),
        (7, 82, 0b1_1000, true),
        (7, 82, 0b0_1001, false),
        (7, 82, 1 << 5, false),
        (7, 82, 1 << 31, false),
    ] {
        let mut gicc = [0; 82];
        gicc[0] = 11;
        gicc[1] = length;
        gicc[12..16].copy_from_slice(&flags.to_le_bytes());
        let firmware = Firmware::new(vec![(
            FIRST,
            madt_with_revision(revision, &gicc[..usize::from(length)]),
        )]);
        let result = firmware.discover();
        if valid {
            assert_eq!(result.unwrap().madt().unwrap().processors().count(), 1);
        } else {
            assert_eq!(result.unwrap_err(), Error::InvalidRecord);
        }
    }
}
