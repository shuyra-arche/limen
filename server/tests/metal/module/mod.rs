#[path = "../../../src/metal/mmu/internal/access.rs"]
#[allow(dead_code)]
pub mod access;
#[path = "../../../src/metal/address.rs"]
pub mod address;
#[path = "../../../src/metal/firmware/devicetree/mod.rs"]
#[allow(dead_code)]
pub mod devicetree;
#[path = "../../../src/metal/module/mod.rs"]
pub mod module;

mod metal {
    pub(crate) mod firmware {
        pub(crate) use crate::devicetree;
    }
}

mod lifecycle;

use access::MappedRange;
use address::{PhysicalAddress, PhysicalRange};
use limen_treaty::model::{ByteLength, PageSize};
use module::{Abi, Contract, DescriptionError, Error, Machine, ModuleImage, Permissions};
use std::mem::MaybeUninit;

const PAGE: PageSize = PageSize::new(4096).unwrap();
const VIRTUAL: u64 = 0x4000_0000;
const PHYSICAL: PhysicalAddress = PhysicalAddress::new(0x20_0000);
const IMAGES: [(Machine, &[u8]); 3] = [
    (
        Machine::Riscv64,
        include_bytes!(concat!(env!("OUT_DIR"), "/riscv64.elf")),
    ),
    (
        Machine::X86_64,
        include_bytes!(concat!(env!("OUT_DIR"), "/x86_64.elf")),
    ),
    (
        Machine::Aarch64,
        include_bytes!(concat!(env!("OUT_DIR"), "/aarch64.elf")),
    ),
];
const SV39_ROOT: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/sv39-root.elf"));

#[derive(Clone, Copy)]
struct Program {
    header: usize,
    kind: u32,
    offset: usize,
    address: usize,
    file_size: usize,
    memory_size: usize,
}

fn word(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

fn programs(bytes: &[u8]) -> impl Iterator<Item = Program> + '_ {
    let start = word(bytes, 32) as usize;
    let length = usize::from(u16::from_le_bytes([bytes[54], bytes[55]]));
    let count = usize::from(u16::from_le_bytes([bytes[56], bytes[57]]));
    (0..count).map(move |index| {
        let header = start + index * length;
        Program {
            header,
            kind: u32::from_le_bytes(bytes[header..header + 4].try_into().unwrap()),
            offset: word(bytes, header + 8) as usize,
            address: word(bytes, header + 16) as usize,
            file_size: word(bytes, header + 32) as usize,
            memory_size: word(bytes, header + 40) as usize,
        }
    })
}

fn program(bytes: &[u8], kind: u32) -> Program {
    programs(bytes)
        .find(|program| program.kind == kind)
        .unwrap()
}

fn note_property(bytes: &[u8], path: &[u8], name: &[u8]) -> usize {
    let note = program(bytes, 4);
    let name_length =
        u32::from_le_bytes(bytes[note.offset..note.offset + 4].try_into().unwrap()) as usize;
    let fdt_length =
        u32::from_le_bytes(bytes[note.offset + 4..note.offset + 8].try_into().unwrap()) as usize;
    let fdt_start = note.offset + 12 + name_length.next_multiple_of(4);
    let description =
        devicetree::DeviceTree::new(&bytes[fdt_start..fdt_start + fdt_length]).unwrap();
    let property = description
        .find_path(path)
        .unwrap()
        .unwrap()
        .property(name)
        .unwrap()
        .unwrap();
    property.as_ptr().addr() - bytes.as_ptr().addr()
}

fn relative_entry(bytes: &[u8]) -> usize {
    let dynamic = program(bytes, 2);
    let virtual_address = bytes[dynamic.offset..dynamic.offset + dynamic.file_size]
        .as_chunks::<16>()
        .0
        .iter()
        .find(|record| word(*record, 0) == 7)
        .map(|record| word(record, 8) as usize)
        .unwrap();
    let load = programs(bytes)
        .find(|program| {
            program.kind == 1
                && (program.address..program.address + program.file_size).contains(&virtual_address)
        })
        .unwrap();
    load.offset + (virtual_address - load.address)
}

fn strip_sections(bytes: &[u8]) -> Vec<u8> {
    let end = programs(bytes)
        .map(|program| program.offset + program.file_size)
        .max()
        .unwrap();
    let mut bytes = bytes[..end].to_vec();
    bytes[40..48].fill(0);
    bytes[58..64].fill(0);
    bytes
}

fn mapped(bytes: &mut [MaybeUninit<u8>]) -> MappedRange<'_> {
    let extent = PHYSICAL.range(ByteLength::new(bytes.len() as u64)).unwrap();
    // This test fixture owns the simulation buffer exclusively. Its byte
    // order models the supplied physical range, independent of its Host address.
    unsafe { MappedRange::new(extent, bytes).unwrap() }
}

#[test]
fn prepares_independently_linked_programs_using_only_program_headers() {
    for (machine, bytes) in IMAGES {
        let bytes = strip_sections(bytes);
        let image = ModuleImage::read(&bytes, machine, PAGE).unwrap();
        let description = image.description();
        assert_eq!(description.abi(), Abi::Scalar);
        let interface = description.interface(Contract::MmuGeometry).unwrap();
        assert_eq!(interface.version(), 1);
        assert_eq!(interface.entry(), word(&bytes, 24));

        let length = image.required_length().as_usize().unwrap();
        let mut buffer = vec![MaybeUninit::new(0xa5); length];
        let mut storage = mapped(&mut buffer);
        let plan = image.prepare(&mut storage, VIRTUAL).unwrap();
        assert_eq!(plan.physical(), storage.physical());
        assert_eq!(plan.virtual_range(), VIRTUAL..VIRTUAL + length as u64);
        assert_eq!(
            plan.entry(Contract::MmuGeometry),
            Some(VIRTUAL + interface.entry())
        );

        let permissions = [
            Permissions::ReadOnly,
            Permissions::ReadExecute,
            Permissions::ReadWrite,
        ];
        let segments = plan.segments().collect::<Vec<_>>();
        assert_eq!(segments.len(), permissions.len());
        for (index, (segment, expected)) in segments.iter().zip(permissions).enumerate() {
            assert_eq!(segment.permissions, expected);
            let offset = index as u64 * PAGE.get();
            let physical = PhysicalRange::new(
                PhysicalAddress::new(PHYSICAL.get() + offset),
                PhysicalAddress::new(PHYSICAL.get() + offset + PAGE.get()),
            )
            .unwrap();
            assert_eq!(segment.physical, physical);
            assert_eq!(
                segment.virtual_range,
                VIRTUAL + offset..VIRTUAL + offset + PAGE.get()
            );
        }

        let bytes_after = storage
            .bytes()
            .iter()
            // The fixture pre-initializes each byte, and prepare writes bytes only.
            .map(|byte| unsafe { byte.assume_init() })
            .collect::<Vec<_>>();
        let relocation = relative_entry(&bytes);
        let site = word(&bytes, relocation) as usize;
        let target = word(&bytes, relocation + 16);
        assert_eq!(word(&bytes_after, site), VIRTUAL + target);
        assert_eq!(word(&bytes_after, target as usize), PAGE.get());

        let code = programs(&bytes)
            .find(|program| program.kind == 1 && program.address == interface.entry() as usize)
            .unwrap();
        assert_eq!(
            bytes_after[code.address..code.address + code.file_size],
            bytes[code.offset..code.offset + code.file_size],
        );
        let data = programs(&bytes)
            .find(|program| {
                program.kind == 1
                    && program.address <= site
                    && site < program.address + program.file_size
            })
            .unwrap();
        assert!(data.memory_size > data.file_size);
        assert!(
            bytes_after[data.address + data.file_size..data.address + data.memory_size]
                .iter()
                .all(|byte| *byte == 0)
        );
    }
}

#[test]
fn rejects_malformed_permissions_note_and_relative_record() {
    let (machine, original) = IMAGES[1];
    let note = program(original, 4);
    let load = program(original, 1);
    let relocation = relative_entry(original);

    let entry_offset = note_property(original, b"/interfaces/mmu-geometry", b"entry");

    // The independently linked fixtures contain a single RELATIVE record.
    let cases: [(usize, &[u8]); 9] = [
        (load.header + 4, &7_u32.to_le_bytes()),
        (load.header + 40, &0_u64.to_le_bytes()),
        (load.header + 40, &(PAGE.get() + 8).to_le_bytes()),
        (note.offset + 8, &2_u32.to_le_bytes()),
        (entry_offset, &0x2000_u64.to_be_bytes()),
        (relocation, &u64::MAX.to_le_bytes()),
        (relocation + 8, &0_u32.to_le_bytes()),
        (relocation + 12, &1_u32.to_le_bytes()),
        (relocation + 16, &u64::MAX.to_le_bytes()),
    ];
    for (offset, replacement) in cases {
        let mut broken = original.to_vec();
        broken[offset..offset + replacement.len()].copy_from_slice(replacement);
        assert!(
            ModuleImage::read(&broken, machine, PAGE).is_err(),
            "corruption at {offset:#x}"
        );
    }
    assert!(ModuleImage::read(original, Machine::Aarch64, PAGE).is_err());
    assert!(ModuleImage::read(&original[..64], machine, PAGE).is_err());
}

#[test]
fn rv64_entry_needs_the_file_bytes_for_its_instruction_width() {
    let (machine, source) = IMAGES[0];
    let mut bytes = source.to_vec();
    let text = programs(source)
        .find(|program| program.kind == 1 && program.address == word(source, 24) as usize)
        .unwrap();
    bytes[48] |= 1;
    bytes[text.header + 32..text.header + 40].copy_from_slice(&2u64.to_le_bytes());
    bytes[text.offset..text.offset + 2].copy_from_slice(&[0x01, 0]);
    assert!(ModuleImage::read(&bytes, machine, PAGE).is_ok());

    bytes[text.offset] = 0x13;
    assert!(matches!(
        ModuleImage::read(&bytes, machine, PAGE),
        Err(Error::Range)
    ));
}

#[test]
fn selects_a_unique_provider_from_the_original_images() {
    let (machine, bytes) = IMAGES[1];
    let select = |source: &[&'static [u8]]| {
        ModuleImage::select(source, machine, PAGE, Contract::MmuGeometry)
    };
    assert!(select(&[]).unwrap().is_none());
    let source = [bytes];
    let selected = select(&source).unwrap().unwrap();
    assert_eq!(selected.description().interfaces().len(), 1);
    assert!(matches!(select(&[bytes; 2]), Err(Error::Conflict)));
    assert!(matches!(select(&[bytes; 17]), Err(Error::Capacity)));
    assert!(select(&[bytes, &bytes[..64]]).is_err());
}

#[test]
fn selects_distinct_rv64_geometry_and_root_images() {
    let (machine, geometry) = IMAGES[0];
    let sources = [geometry, SV39_ROOT];
    for (bytes, abi, own, other) in [
        (
            geometry,
            Abi::Scalar,
            Contract::MmuGeometry,
            Contract::Sv39Root,
        ),
        (
            SV39_ROOT,
            Abi::Sv39Root,
            Contract::Sv39Root,
            Contract::MmuGeometry,
        ),
    ] {
        let selected = ModuleImage::select(&sources, machine, PAGE, own)
            .unwrap()
            .unwrap();
        let description = selected.description();
        assert_eq!(description.abi(), abi);
        assert!(description.interface(other).is_none());
        let offered = description.interface(own).unwrap();
        assert_eq!(offered.entry(), word(bytes, 24));
        assert_eq!(offered.version(), 1);
        assert!(matches!(
            ModuleImage::select(&[geometry, SV39_ROOT, bytes], machine, PAGE, own),
            Err(Error::Conflict)
        ));
    }
}

#[test]
fn rejects_a_mismatched_root_abi_machine_or_version() {
    for (machine, bytes, path, property, replacement) in [
        (
            Machine::Riscv64,
            SV39_ROOT,
            b"/".as_slice(),
            b"abi".as_slice(),
            1_u32,
        ),
        (
            Machine::Riscv64,
            SV39_ROOT,
            b"/interfaces/sv39-root",
            b"version",
            2,
        ),
        (Machine::X86_64, IMAGES[1].1, b"/", b"abi", 2),
    ] {
        let offset = note_property(bytes, path, property);
        let mut modified = bytes.to_vec();
        modified[offset..offset + 4].copy_from_slice(&replacement.to_be_bytes());
        assert!(matches!(
            ModuleImage::read(&modified, machine, PAGE),
            Err(Error::Description(DescriptionError::Unsupported))
        ));
    }
}

#[test]
fn failed_placement_preserves_the_storage() {
    let (machine, bytes) = IMAGES[0];
    let image = ModuleImage::read(bytes, machine, PAGE).unwrap();
    let length = image.required_length().as_usize().unwrap();
    for (size, start) in [
        (length - 1, VIRTUAL),
        (length, VIRTUAL + 1),
        (length, u64::MAX - (PAGE.get() - 1)),
    ] {
        let mut buffer = vec![MaybeUninit::new(0x5a); size];
        let mut storage = mapped(&mut buffer);
        assert!(matches!(
            image.prepare(&mut storage, start),
            Err(Error::Range)
        ));
        assert!(
            storage
                .bytes()
                .iter()
                // A rejected placement must leave the initialized sentinels intact.
                .all(|byte| unsafe { byte.assume_init() } == 0x5a)
        );
    }
}
