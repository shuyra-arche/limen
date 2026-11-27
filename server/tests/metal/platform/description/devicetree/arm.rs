//! Interpret Arm platform descriptions on the ordinary Host, without native calls.
use super::fixture::Fixture;
use kernel::metal::{
    chrono::aarch64::GenericCounter,
    interrupt::aarch64::{
        gic::{GicV3, Trigger, redistributor::PrivateInterrupts},
        internal::timer::GenericTimer,
    },
    platform::{
        description::devicetree::{DeviceTree, Error},
        firmware::psci::{Conduit, PsciDescription, PsciVersion},
    },
    processor::hart::aarch64::description::Aarch64Cpus,
};

fn cpus(affinities: &[u64]) -> Vec<u8> {
    let mut fixture = Fixture::root();
    fixture.node(b"cpus");
    fixture.cells(b"#address-cells", &[2]);
    fixture.cells(b"#size-cells", &[0]);
    for (index, affinity) in affinities.iter().enumerate() {
        let name = format!("cpu@{index}");
        fixture.node(name.as_bytes());
        fixture.property(b"device_type", b"cpu\0");
        fixture.cells(b"reg", &[(*affinity >> 32) as u32, *affinity as u32]);
        fixture.property(b"enable-method", b"psci\0");
        if index != 0 {
            fixture.property(b"status", b"disabled\0");
        }
        fixture.end();
    }
    fixture.end();
    fixture.finish()
}

#[test]
fn affinity_descriptions_keep_their_original_scope() {
    let bytes = cpus(&[0, 0x1_0000_0001]);
    let tree = DeviceTree::new(&bytes).unwrap();
    let scope = Aarch64Cpus::read(&tree).unwrap().unwrap();
    let candidates: Vec<_> = scope.candidates().collect();
    assert_eq!(candidates.len(), 2);
    assert_eq!(candidates[0].affinity, 0);
    assert!(candidates[0].enabled);
    assert!(candidates[0].starts_with_psci);
    assert_eq!(candidates[1].affinity, 0x1_0000_0001);
    assert!(!candidates[1].enabled);
    assert_ne!(candidates[0].source, candidates[1].source);

    for (affinities, error) in [([0, 0], Error::Conflict), ([0, 1 << 24], Error::Range)] {
        let bytes = cpus(&affinities);
        let tree = DeviceTree::new(&bytes).unwrap();
        assert_eq!(Aarch64Cpus::read(&tree).err(), Some(error));
    }
}

fn psci(compatible: &[u8], method: &[u8], duplicate: bool) -> Vec<u8> {
    let mut fixture = Fixture::root();
    for name in [b"psci".as_slice(), b"psci-other".as_slice()]
        .into_iter()
        .take(if duplicate { 2 } else { 1 })
    {
        fixture.node(name);
        fixture.property(b"compatible", compatible);
        fixture.property(b"method", method);
        fixture.end();
    }
    fixture.finish()
}

#[test]
fn psci_description_is_not_a_live_conduit_probe() {
    let bytes = psci(b"arm,psci-1.0\0", b"hvc\0", false);
    let tree = DeviceTree::new(&bytes).unwrap();
    let description = PsciDescription::read(&tree).unwrap().unwrap();
    assert_eq!(description.conduit(), Conduit::Hvc);
    assert_eq!(
        description.minimum_version(),
        PsciVersion { major: 1, minor: 0 }
    );
    let bytes = psci(b"arm,psci-0.2\0", b"smc\0", false);
    let tree = DeviceTree::new(&bytes).unwrap();
    let description = PsciDescription::read(&tree).unwrap().unwrap();
    assert_eq!(description.conduit(), Conduit::Smc);
    assert_eq!(
        description.minimum_version(),
        PsciVersion { major: 0, minor: 2 }
    );

    for (method, duplicate, error) in [
        (b"unsupported\0".as_slice(), false, Error::Unsupported),
        (b"hvc\0".as_slice(), true, Error::Conflict),
    ] {
        let bytes = psci(b"arm,psci-1.0\0", method, duplicate);
        let tree = DeviceTree::new(&bytes).unwrap();
        assert_eq!(PsciDescription::read(&tree).err(), Some(error));
    }
}

fn timer(virtual_ppi: u32) -> Vec<u8> {
    timer_with(
        &[1, 13, 4, 1, 14, 4, 1, virtual_ppi, 4, 1, 10, 4],
        None,
        false,
    )
}

fn timer_with(interrupts: &[u32], names: Option<&[u8]>, extended: bool) -> Vec<u8> {
    timer_with_properties(interrupts, names, extended, 62_500_000, b"")
}

fn timer_with_properties(
    interrupts: &[u32],
    names: Option<&[u8]>,
    extended: bool,
    frequency: u32,
    always_on: &[u8],
) -> Vec<u8> {
    let mut fixture = Fixture::root();
    fixture.node(b"gic@8000000");
    fixture.property(b"compatible", b"arm,gic-v3\0");
    fixture.property(b"interrupt-controller", b"");
    fixture.cells(b"#interrupt-cells", &[3]);
    fixture.cells(b"phandle", &[7]);
    fixture.cells(b"reg", &[0x0800_0000, 0x1_0000, 0x080a_0000, 0x2_0000]);
    fixture.end();
    fixture.node(b"timer");
    fixture.property(b"compatible", b"arm,armv8-timer\0");
    fixture.cells(b"interrupt-parent", &[7]);
    if extended {
        fixture.cells(b"interrupts-extended", interrupts);
    } else {
        fixture.cells(b"interrupts", interrupts);
    }
    if let Some(names) = names {
        fixture.property(b"interrupt-names", names);
    }
    fixture.cells(b"clock-frequency", &[frequency]);
    fixture.property(b"always-on", always_on);
    fixture.end();
    fixture.finish()
}

#[test]
fn timer_description_resolves_its_actual_interrupt_domain() {
    let bytes = timer(11);
    let tree = DeviceTree::new(&bytes).unwrap();
    let counter = GenericCounter::read(&tree).unwrap().unwrap();
    let interrupts = PrivateInterrupts::read(
        &tree,
        counter.source(),
        GicV3::read(&tree).unwrap().unwrap(),
    )
    .unwrap();
    let timer = GenericTimer::describe(&tree, &interrupts).unwrap();
    let controller = interrupts.controller();
    assert_eq!(timer.source(), counter.source());
    assert_eq!(timer.domain(), controller.source());
    assert_eq!(timer.virtual_interrupt().intid, 27);
    assert_eq!(timer.virtual_interrupt().trigger, Trigger::LevelHigh);
    assert_eq!(controller.distributor().start().get(), 0x0800_0000);
    assert_eq!(controller.redistributor().length().get(), 0x2_0000);
    assert_eq!(counter.described_frequency().unwrap().get(), 62_500_000);
    assert!(timer.always_on());
    assert_eq!(controller.source(), tree.phandle(7).unwrap().unwrap().id());
}

#[test]
fn invalid_ppi_does_not_invalidate_counter_description() {
    for (ppi, error) in [(16, Error::Range), (13, Error::Conflict)] {
        let bytes = timer(ppi);
        let tree = DeviceTree::new(&bytes).unwrap();
        let counter = GenericCounter::read(&tree).unwrap().unwrap();
        assert_eq!(counter.described_frequency().unwrap().get(), 62_500_000);
        assert_eq!(
            PrivateInterrupts::read(
                &tree,
                counter.source(),
                GicV3::read(&tree).unwrap().unwrap()
            )
            .err(),
            Some(error)
        );
    }
}

#[test]
fn timer_roles_select_only_from_the_checked_interrupt_list() {
    let bytes = timer_with(&[7, 1, 14, 4, 7, 1, 11, 1], Some(b"phys\0virt\0"), true);
    let tree = DeviceTree::new(&bytes).unwrap();
    let counter = GenericCounter::read(&tree).unwrap().unwrap();
    let interrupts = PrivateInterrupts::read(
        &tree,
        counter.source(),
        GicV3::read(&tree).unwrap().unwrap(),
    )
    .unwrap();
    let timer = GenericTimer::describe(&tree, &interrupts).unwrap();
    assert_eq!(timer.virtual_interrupt().intid, 27);
    assert_eq!(timer.virtual_interrupt().trigger, Trigger::EdgeRising);

    let foreign = bytes.clone();
    let foreign_tree = DeviceTree::new(&foreign).unwrap();
    assert_eq!(
        PrivateInterrupts::read(
            &foreign_tree,
            counter.source(),
            GicV3::read(&foreign_tree).unwrap().unwrap()
        )
        .err(),
        Some(Error::Missing)
    );
    assert_eq!(
        GenericTimer::describe(&foreign_tree, &interrupts).err(),
        Some(Error::Missing)
    );

    let bytes = timer_with(&[1, 14, 4, 1, 11, 4], None, false);
    let tree = DeviceTree::new(&bytes).unwrap();
    let counter = GenericCounter::read(&tree).unwrap().unwrap();
    let interrupts = PrivateInterrupts::read(
        &tree,
        counter.source(),
        GicV3::read(&tree).unwrap().unwrap(),
    )
    .unwrap();
    assert_eq!(
        GenericTimer::describe(&tree, &interrupts).err(),
        Some(Error::Missing)
    );

    let bytes = timer_with(
        &[1, 1, 4, 1, 2, 4, 1, 3, 4, 1, 4, 4, 1, 5, 4, 1, 6, 4],
        None,
        false,
    );
    let tree = DeviceTree::new(&bytes).unwrap();
    let counter = GenericCounter::read(&tree).unwrap().unwrap();
    let interrupts = PrivateInterrupts::read(
        &tree,
        counter.source(),
        GicV3::read(&tree).unwrap().unwrap(),
    )
    .unwrap();
    assert_eq!(
        GenericTimer::describe(&tree, &interrupts).err(),
        Some(Error::Unsupported)
    );

    let bytes = timer_with(&[7, 1, 14, 4, 8, 1, 11, 4], Some(b"phys\0virt\0"), true);
    let tree = DeviceTree::new(&bytes).unwrap();
    let counter = GenericCounter::read(&tree).unwrap().unwrap();
    assert_eq!(
        PrivateInterrupts::read(
            &tree,
            counter.source(),
            GicV3::read(&tree).unwrap().unwrap()
        )
        .err(),
        Some(Error::Unsupported)
    );
}

#[test]
fn counter_and_timer_validate_only_their_own_facts() {
    let specifiers = [1, 13, 4, 1, 14, 4, 1, 11, 4];
    let bytes = timer_with_properties(&specifiers, None, false, 62_500_000, b"invalid");
    let tree = DeviceTree::new(&bytes).unwrap();
    let counter = GenericCounter::read(&tree).unwrap().unwrap();
    let interrupts = PrivateInterrupts::read(
        &tree,
        counter.source(),
        GicV3::read(&tree).unwrap().unwrap(),
    )
    .unwrap();
    assert_eq!(
        GenericTimer::describe(&tree, &interrupts).err(),
        Some(Error::Format)
    );

    let bytes = timer_with_properties(&specifiers, None, false, 0, b"");
    let tree = DeviceTree::new(&bytes).unwrap();
    assert_eq!(GenericCounter::read(&tree).err(), Some(Error::Range));
    let source = tree.find_path(b"/timer").unwrap().unwrap().id();
    let interrupts =
        PrivateInterrupts::read(&tree, source, GicV3::read(&tree).unwrap().unwrap()).unwrap();
    assert_eq!(
        GenericTimer::describe(&tree, &interrupts)
            .unwrap()
            .virtual_interrupt()
            .intid,
        27
    );
}

#[test]
fn shared_routes_retain_their_domain_and_exclude_private_and_special_ids() {
    use kernel::metal::interrupt::aarch64::gic::distributor::Route;
    for (entry, expected) in [
        ([0, 0, 4], Ok((32, Trigger::LevelHigh))),
        ([0, 987, 1], Ok((1019, Trigger::EdgeRising))),
        ([1, 0, 4], Err(Error::Unsupported)),
        ([0, 988, 4], Err(Error::Range)),
        ([0, 0, 8], Err(Error::Unsupported)),
    ] {
        for extended in [false, true] {
            let mut cells = Vec::new();
            if extended {
                cells.push(7);
            }
            cells.extend(entry);
            let bytes = timer_with(&cells, None, extended);
            let tree = DeviceTree::new(&bytes).unwrap();
            let gic = GicV3::read(&tree).unwrap().unwrap();
            let device = tree.find_path(b"/timer").unwrap().unwrap().id();
            assert_eq!(
                Route::read(&tree, device, gic, 0).map(|route| (route.intid(), route.trigger())),
                expected
            );
            assert_eq!(
                Route::read(&tree, device, gic, 1).unwrap_err(),
                Error::Range
            );
            let other = bytes.clone();
            let other = DeviceTree::new(&other).unwrap();
            assert_eq!(
                Route::read(&other, device, gic, 0).unwrap_err(),
                Error::Missing
            );
        }
    }
}
