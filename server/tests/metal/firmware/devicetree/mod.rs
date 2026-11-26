#[path = "../../../../src/metal/address.rs"]
pub mod address;
#[path = "../../../../src/metal/firmware/devicetree/mod.rs"]
pub mod devicetree;

use devicetree::{DeviceTree, Error, memory};

#[derive(Default)]
struct Fixture {
    structure: Vec<u8>,
    strings: Vec<u8>,
}

impl Fixture {
    fn token(&mut self, value: u32) {
        self.structure.extend(value.to_be_bytes());
    }

    fn padded(&mut self, bytes: &[u8]) {
        self.structure.extend(bytes);
        self.structure
            .resize(self.structure.len().next_multiple_of(4), 0);
    }

    fn node(&mut self, name: &[u8]) {
        self.token(1);
        self.padded(&[name, b"\0"].concat());
    }

    fn property(&mut self, name: &[u8], value: &[u8]) {
        self.token(3);
        self.token(value.len().try_into().unwrap());
        self.token(self.strings.len().try_into().unwrap());
        self.strings.extend([name, b"\0"].concat());
        self.padded(value);
    }

    fn cells(&mut self, name: &[u8], values: &[u32]) {
        self.property(
            name,
            &values
                .iter()
                .flat_map(|value| value.to_be_bytes())
                .collect::<Vec<_>>(),
        );
    }

    fn end(&mut self) {
        self.token(2);
    }

    fn root() -> Self {
        let mut fixture = Self::default();
        fixture.node(b"");
        fixture.cells(b"#address-cells", &[1]);
        fixture.cells(b"#size-cells", &[1]);
        fixture
    }

    fn finish(mut self) -> Vec<u8> {
        self.end();
        self.token(9);
        let structure = 56;
        let strings = structure + self.structure.len();
        let mut bytes = Vec::new();
        for value in [
            0xd00d_feed,
            strings + self.strings.len(),
            structure,
            strings,
            40,
            17,
            16,
            0,
            self.strings.len(),
            self.structure.len(),
        ] {
            bytes.extend(u32::try_from(value).unwrap().to_be_bytes());
        }
        bytes.extend([0; 16]);
        bytes.extend(self.structure);
        bytes.extend(self.strings);
        bytes
    }
}

#[test]
fn resolves_nodes_and_properties_against_their_exact_source() {
    let mut fixture = Fixture::root();
    fixture.node(b"controller@0");
    fixture.cells(b"phandle", &[7]);
    fixture.property(b"compatible", b"test,controller\0simple-bus\0");
    fixture.end();
    let bytes = fixture.finish();
    let identical_bytes = bytes.clone();
    let tree = DeviceTree::new(&bytes).unwrap();
    let other = DeviceTree::new(&identical_bytes).unwrap();
    let controller = tree.find_path(b"/controller@0").unwrap().unwrap();
    let root = tree.find_path(b"/").unwrap().unwrap();
    assert_eq!(controller.parent_id(), Some(root.id()));
    assert_eq!(tree.phandle(7).unwrap().unwrap().id(), controller.id());
    assert!(tree.phandle(9).unwrap().is_none());
    assert!(controller.compatible(b"simple-bus").unwrap());
    assert!(!controller.compatible(b"test,missing").unwrap());
    assert!(other.node(controller.id()).is_none());
    assert_eq!(
        &bytes[controller.id().offset()..][..4],
        &1_u32.to_be_bytes()
    );
    let phandle = controller.property_entry(b"phandle").unwrap().unwrap();
    assert_eq!(&bytes[phandle.offset()..][..4], &3_u32.to_be_bytes());
    assert_eq!(phandle.value(), &7_u32.to_be_bytes());
}

#[test]
fn rejects_duplicate_properties_and_ambiguous_references() {
    let mut fixture = Fixture::root();
    fixture.property(b"compatible", b"one\0");
    fixture.property(b"compatible", b"two\0");
    assert_eq!(
        DeviceTree::new(&fixture.finish()).unwrap_err(),
        Error::Conflict
    );

    let mut fixture = Fixture::root();
    for name in [b"controller@0", b"controller@1"] {
        fixture.node(name);
        fixture.cells(b"phandle", &[7]);
        fixture.end();
    }
    let blob = fixture.finish();
    let tree = DeviceTree::new(&blob).unwrap();
    assert_eq!(tree.phandle(7).unwrap_err(), Error::Conflict);
    assert_eq!(tree.phandle(0).unwrap_err(), Error::Format);

    let mut fixture = Fixture::root();
    fixture.property(b"phandle", &[7]);
    assert_eq!(
        DeviceTree::new(&fixture.finish()).unwrap_err(),
        Error::Format
    );

    let mut fixture = Fixture::root();
    fixture.cells(b"phandle", &[7]);
    fixture.cells(b"linux,phandle", &[8]);
    assert_eq!(
        DeviceTree::new(&fixture.finish()).unwrap_err(),
        Error::Conflict
    );
}

#[test]
fn bounds_property_padding_and_validates_node_padding() {
    let mut fixture = Fixture::root();
    let name_padding = fixture.structure.len() + 4 + b"test\0".len();
    fixture.node(b"test");
    let value_padding = fixture.structure.len() + 12 + 1;
    fixture.property(b"data", &[42]);
    fixture.end();
    let mut bytes = fixture.finish();
    let structure = u32::from_be_bytes(bytes[8..12].try_into().unwrap()) as usize;
    bytes[structure + value_padding] = 1;
    let tree = DeviceTree::new(&bytes).unwrap();
    let node = tree.find_path(b"/test").unwrap().unwrap();
    assert_eq!(node.property(b"data").unwrap(), Some(&[42][..]));

    bytes[structure + name_padding] = 1;
    assert_eq!(DeviceTree::new(&bytes).unwrap_err(), Error::Format);
}

fn bus(outer: Option<&[u32]>, inner: Option<&[u32]>, register: &[u32]) -> Vec<u8> {
    let mut fixture = Fixture::root();
    fixture.node(b"outer");
    fixture.property(b"compatible", b"simple-bus\0");
    fixture.cells(b"#address-cells", &[1]);
    fixture.cells(b"#size-cells", &[1]);
    if let Some(outer) = outer {
        fixture.cells(b"ranges", outer);
    }
    fixture.node(b"inner");
    fixture.property(b"compatible", b"test,inner\0simple-bus\0");
    fixture.cells(b"#address-cells", &[1]);
    fixture.cells(b"#size-cells", &[1]);
    if let Some(inner) = inner {
        fixture.cells(b"ranges", inner);
    }
    fixture.node(b"device@10");
    fixture.cells(b"reg", register);
    fixture.end();
    fixture.end();
    fixture.end();
    fixture.finish()
}

fn translated(input: &[u8]) -> Result<(u64, u64), Error> {
    let tree = DeviceTree::new(input)?;
    let node = tree.find_path(b"/outer/inner/device@10")?.unwrap();
    let range = node.register(0)?;
    Ok((range.start().get(), range.end().get()))
}

#[test]
fn translates_nested_windows_and_requires_each_parent_mapping() {
    let outer = [0x100, 0x1000, 0x100];
    let inner = [0, 0x100, 0x80];
    assert_eq!(
        translated(&bus(Some(&outer), Some(&inner), &[0x10, 0x10])),
        Ok((0x1010, 0x1020))
    );
    assert_eq!(
        translated(&bus(Some(&outer), Some(&[]), &[0x110, 0x10])),
        Ok((0x1010, 0x1020))
    );
    assert_eq!(
        translated(&bus(Some(&outer), None, &[0x110, 0x10])),
        Err(Error::Missing)
    );
    assert_eq!(
        translated(&bus(Some(&outer), Some(&inner), &[0x70, 0x20])),
        Err(Error::Range)
    );
    let overlapping = [0, 0x100, 0x80, 0x15, 0x120, 0x20];
    assert_eq!(
        translated(&bus(Some(&outer), Some(&overlapping), &[0x10, 0x10])),
        Err(Error::Conflict)
    );
    assert_eq!(
        translated(&bus(Some(&outer), Some(&inner), &[u32::MAX, 2])),
        Err(Error::Range)
    );
}

#[test]
fn rejects_registers_in_an_unsupported_address_domain() {
    let mut fixture = Fixture::root();
    fixture.node(b"isa");
    fixture.cells(b"#address-cells", &[2]);
    fixture.cells(b"#size-cells", &[1]);
    fixture.cells(b"ranges", &[1, 0, 0x1000, 0x20]);
    fixture.node(b"serial@8");
    fixture.cells(b"reg", &[1, 8, 4]);
    fixture.end();
    fixture.end();
    let bytes = fixture.finish();
    let tree = DeviceTree::new(&bytes).unwrap();
    let serial = tree.find_path(b"/isa/serial@8").unwrap().unwrap();
    assert_eq!(serial.register(0), Err(Error::Unsupported));
}

#[test]
fn keeps_the_trusted_dtb_and_memory_descriptions_on_one_view() {
    #[repr(align(8))]
    struct Storage([u8; 4096]);

    let mut fixture = Fixture::root();
    fixture.node(b"memory@80000000");
    fixture.property(b"device_type", b"memory\0");
    fixture.cells(b"reg", &[0x8000_0000, 0x4000]);
    fixture.end();
    let source = fixture.finish();
    let mut storage = Storage([0; 4096]);
    storage.0[..source.len()].copy_from_slice(&source);
    let start = storage.0.as_ptr().addr();
    let mut memory = None;
    let tree = unsafe {
        memory::read(
            storage.0.as_ptr().cast(),
            start..start + storage.0.len(),
            0..0,
            &mut memory,
        )
    }
    .unwrap();
    assert_eq!(tree.bytes().as_ptr().addr(), start);
    assert_eq!(tree.bytes().len(), source.len());
    let mut reservations_only = None;
    memory::describe_reservations(tree, &mut reservations_only).unwrap();
    assert_eq!(reservations_only.unwrap().ram().len(), 0);

    assert_eq!(
        memory.unwrap().ram().next(),
        Some(
            tree.find_path(b"/memory@80000000")
                .unwrap()
                .unwrap()
                .register(0)
                .unwrap()
        )
    );
}

mod riscv;

mod metal {
    pub(crate) mod firmware {
        pub(crate) use crate::devicetree;
    }
    pub(crate) mod hart {
        pub(crate) use crate::riscv::hart_description as description;
    }
}
