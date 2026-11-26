use kernel::metal::platform::firmware::devicetree::{DeviceTree, Error};

mod fixture;
use fixture::Fixture;

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
fn permits_the_exclusive_end_of_a_scalar_32_bit_bus() {
    assert_eq!(
        translated(&bus(Some(&[]), Some(&[]), &[u32::MAX, 1])),
        Ok((u64::from(u32::MAX), 1_u64 << 32))
    );
    assert_eq!(
        translated(&bus(Some(&[]), Some(&[]), &[u32::MAX, 2])),
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

mod riscv;

mod arm;
