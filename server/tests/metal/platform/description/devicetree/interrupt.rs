//! Physical target indices, source scope and machine timer geometry.
use super::Fixture;
use kernel::metal::processor::hart::riscv64::description::RiscvCpus;
use kernel::metal::{
    interrupt::{external::aplic, internal::aclint::timer},
    platform::description::devicetree::{DeviceTree, Error},
};

fn fixture(
    aplic_targets: &[u32],
    timer_targets: &[u32],
    source: u32,
    parent: u32,
    span: u32,
) -> Vec<u8> {
    let mut fixture = Fixture::root();
    fixture.node(b"cpus");
    fixture.cells(b"#address-cells", &[1]);
    fixture.cells(b"#size-cells", &[0]);
    fixture.cells(b"timebase-frequency", &[10_000_000]);
    for (hart, handle) in [(3, 1), (11, 2)] {
        let name = format!("cpu@{hart:x}");
        fixture.node(name.as_bytes());
        fixture.property(b"device_type", b"cpu\0");
        fixture.cells(b"reg", &[hart]);
        fixture.node(b"interrupt-controller");
        fixture.property(b"compatible", b"riscv,cpu-intc\0");
        fixture.property(b"interrupt-controller", b"");
        fixture.cells(b"#interrupt-cells", &[1]);
        fixture.cells(b"phandle", &[handle]);
        fixture.end();
        fixture.end();
    }
    fixture.end();

    for (name, base, handle, targets) in [
        (
            b"aplic@c000000".as_slice(),
            0x0c00_0000,
            4,
            &[2, 11, 1, 11][..],
        ),
        (b"aplic@d000000".as_slice(), 0x0d00_0000, 5, aplic_targets),
    ] {
        fixture.node(name);
        fixture.property(b"compatible", b"riscv,aplic\0");
        fixture.property(b"interrupt-controller", b"");
        fixture.cells(b"#interrupt-cells", &[2]);
        fixture.cells(b"phandle", &[handle]);
        fixture.cells(b"reg", &[base, span]);
        fixture.cells(b"riscv,num-sources", &[96]);
        fixture.cells(b"interrupts-extended", targets);
        fixture.end();
    }

    fixture.node(b"mtimer@2004000");
    fixture.property(b"compatible", b"riscv,aclint-mtimer\0");
    fixture.property(b"status", b"disabled\0");
    fixture.cells(b"reg", &[0x0200_bff8, 0x4008, 0x0200_4000, 16]);
    fixture.cells(b"interrupts-extended", timer_targets);
    fixture.end();

    fixture.node(b"device");
    fixture.cells(b"interrupt-parent", &[parent]);
    fixture.cells(b"interrupts", &[source, 4]);
    fixture.end();
    fixture.finish()
}

#[test]
fn sparse_hart_ids_follow_the_actual_controller_order() {
    let bytes = fixture(&[2, 9, 1, 9], &[2, 7, 1, 7], 10, 5, 0x8000);
    let tree = DeviceTree::new(&bytes).unwrap();
    let cpus = RiscvCpus::new(tree).unwrap().unwrap();
    let aplic = aplic::Description::read(&tree, &cpus, 3).unwrap().unwrap();
    assert_eq!(aplic.hart(), 3);
    assert_eq!(aplic.index(), 1);
    assert_eq!(aplic.contexts(), 2);
    assert_eq!(aplic.registers().start().get(), 0x0d00_0000);
    let device = tree.find_path(b"/device").unwrap().unwrap();
    let route = aplic.route(&tree, device).unwrap();
    assert_eq!(route.domain(), aplic.source());
    assert_eq!(route.id(), 10);
    assert_eq!(route.mode(), aplic::Mode::LevelHigh);
    let timer = timer::Description::read(&tree, &cpus, 3).unwrap().unwrap();
    assert_eq!(timer.counter().start().get(), 0x0200_bff8);
    assert_eq!(timer.comparator().start().get(), 0x0200_4008);
    assert_eq!(timer.comparator().length().get(), 8);
    assert_eq!(
        timer.clock_scope(),
        tree.find_path(b"/cpus").unwrap().unwrap().id()
    );
}

#[test]
fn malformed_or_mixed_targets_are_not_admitted() {
    for (targets, error) in [
        (&[1, 9, 1, 9][..], Error::Conflict),
        (&[1, 9, 2, 11][..], Error::Unsupported),
        (&[1, 9, 99, 9][..], Error::Missing),
        (&[1, 9, 2][..], Error::Format),
    ] {
        let bytes = fixture(targets, &[2, 7, 1, 7], 10, 5, 0x8000);
        let tree = DeviceTree::new(&bytes).unwrap();
        let cpus = RiscvCpus::new(tree).unwrap().unwrap();
        assert_eq!(aplic::Description::read(&tree, &cpus, 3).err(), Some(error));
    }
    for (targets, error) in [
        (&[1, 7, 1, 7][..], Error::Conflict),
        (&[1, 7, 2, 5][..], Error::Unsupported),
        (&[1, 7, 99, 7][..], Error::Missing),
    ] {
        let bytes = fixture(&[2, 9, 1, 9], targets, 10, 5, 0x8000);
        let tree = DeviceTree::new(&bytes).unwrap();
        let cpus = RiscvCpus::new(tree).unwrap().unwrap();
        assert_eq!(timer::Description::read(&tree, &cpus, 3).err(), Some(error));
    }
}

#[test]
fn routes_and_windows_are_checked_before_native_access() {
    for (source, parent, error) in [
        (0, 5, Error::Range),
        (97, 5, Error::Range),
        (10, 4, Error::Conflict),
    ] {
        let bytes = fixture(&[2, 9, 1, 9], &[2, 7, 1, 7], source, parent, 0x8000);
        let tree = DeviceTree::new(&bytes).unwrap();
        let cpus = RiscvCpus::new(tree).unwrap().unwrap();
        let description = aplic::Description::read(&tree, &cpus, 3).unwrap().unwrap();
        let device = tree.find_path(b"/device").unwrap().unwrap();
        assert_eq!(description.route(&tree, device).err(), Some(error));
    }
    let bytes = fixture(&[2, 9, 1, 9], &[2, 7, 1, 7], 10, 5, 0x4000);
    let tree = DeviceTree::new(&bytes).unwrap();
    let cpus = RiscvCpus::new(tree).unwrap().unwrap();
    assert_eq!(
        aplic::Description::read(&tree, &cpus, 3).err(),
        Some(Error::Range)
    );
}

#[test]
fn controller_discovery_rejects_foreign_cpu_scope() {
    let bytes = fixture(&[2, 9, 1, 9], &[2, 7, 1, 7], 10, 5, 0x8000);
    let other = bytes.clone();
    let tree = DeviceTree::new(&bytes).unwrap();
    let cpus = RiscvCpus::new(DeviceTree::new(&other).unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(
        aplic::Description::read(&tree, &cpus, 3).err(),
        Some(Error::Conflict)
    );
    assert_eq!(
        timer::Description::read(&tree, &cpus, 3).err(),
        Some(Error::Conflict)
    );
}

#[test]
fn plic_supervisor_route_preserves_holes_and_rejects_ambiguous_contexts() {
    use kernel::metal::interrupt::external::plic::Route;
    for (targets, index) in [
        (vec![1, u32::MAX, 1, 9], Ok(1)),
        (vec![1, 11, 1, 9], Ok(1)),
        (vec![1, 9, 1, 9], Err(Error::Conflict)),
        (vec![1, 11], Err(Error::Missing)),
        (vec![1, 9, 1], Err(Error::Format)),
    ] {
        let mut fixture = Fixture::root();
        fixture.node(b"cpus");
        fixture.cells(b"#address-cells", &[1]);
        fixture.cells(b"#size-cells", &[0]);
        fixture.node(b"cpu@3");
        fixture.property(b"device_type", b"cpu\0");
        fixture.cells(b"reg", &[3]);
        fixture.node(b"interrupt-controller");
        fixture.property(b"compatible", b"riscv,cpu-intc\0");
        fixture.cells(b"#interrupt-cells", &[1]);
        fixture.property(b"interrupt-controller", b"");
        fixture.cells(b"phandle", &[1]);
        fixture.end();
        fixture.end();
        fixture.end();
        fixture.node(b"plic");
        fixture.property(b"compatible", b"riscv,plic0\0");
        fixture.property(b"interrupt-controller", b"");
        fixture.cells(b"#interrupt-cells", &[1]);
        fixture.cells(b"phandle", &[2]);
        fixture.cells(b"reg", &[0x0c00_0000, 0x40_0000]);
        fixture.cells(b"riscv,ndev", &[31]);
        fixture.cells(b"interrupts-extended", &targets);
        fixture.end();
        fixture.node(b"uart");
        fixture.cells(b"interrupt-parent", &[2]);
        fixture.cells(b"interrupts", &[10]);
        fixture.end();
        let bytes = fixture.finish();
        let tree = DeviceTree::new(&bytes).unwrap();
        let cpus = RiscvCpus::new(tree).unwrap().unwrap();
        let device = tree.find_path(b"/uart").unwrap().unwrap();
        assert_eq!(
            Route::read(&tree, &cpus, device, 3).map(|r| r.context()),
            index
        );
    }
}
