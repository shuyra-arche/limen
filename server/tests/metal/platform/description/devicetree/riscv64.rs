use super::{DeviceTree, Error, Fixture};

use kernel::metal::{
    chrono::riscv64::RiscvTimebase,
    memory::mmu::riscv64::{
        internal::description::{MaximumMode, RiscvMmu},
        sv::Mode,
    },
    processor::execution::description::riscv64::{RiscvExtension, RiscvIsa},
    processor::hart::riscv64::description::RiscvCpus,
};

fn cpu_scope() -> Fixture {
    let mut fixture = Fixture::root();
    fixture.node(b"cpus");
    fixture.cells(b"#address-cells", &[2]);
    fixture.cells(b"#size-cells", &[0]);
    fixture
}

fn start_cpu(fixture: &mut Fixture, name: &[u8], reg: &[u32]) {
    fixture.node(name);
    fixture.property(b"device_type", b"cpu\0");
    fixture.cells(b"reg", reg);
}

fn read_scope(bytes: &[u8]) -> RiscvCpus<'_> {
    RiscvCpus::new(DeviceTree::new(bytes).unwrap())
        .unwrap()
        .unwrap()
}

#[test]
fn rv64_mmu_description_bounds_the_supported_paged_modes() {
    for (property, maximum, supported) in [
        (b"riscv,none\0", MaximumMode::Bare, [false, false, false]),
        (
            b"riscv,sv39\0",
            MaximumMode::Paged(Mode::Sv39),
            [true, false, false],
        ),
        (
            b"riscv,sv48\0",
            MaximumMode::Paged(Mode::Sv48),
            [true, true, false],
        ),
        (
            b"riscv,sv57\0",
            MaximumMode::Paged(Mode::Sv57),
            [true, true, true],
        ),
    ] {
        let mut fixture = cpu_scope();
        start_cpu(&mut fixture, b"cpu@0", &[0, 0]);
        fixture.property(b"mmu-type", property);
        fixture.end();
        fixture.end();
        let bytes = fixture.finish();
        let scope = read_scope(&bytes);
        let cpu = scope.cpu(0).unwrap().unwrap();
        let mmu = RiscvMmu::describe(&cpu).unwrap().unwrap();
        assert_eq!(mmu.maximum(), maximum);
        assert_eq!(
            [Mode::Sv39, Mode::Sv48, Mode::Sv57].map(|mode| maximum.supports(mode)),
            supported
        );
    }
}

#[test]
fn rejects_contradictory_sstc_declarations_for_the_same_hart() {
    let cases = [
        (b"i\0sstc\0".as_slice(), b"rv64i\0".as_slice()),
        (b"i\0".as_slice(), b"rv64i_sstc\0".as_slice()),
    ];
    let results = cases.map(|(modern, legacy)| {
        let mut fixture = cpu_scope();
        start_cpu(&mut fixture, b"cpu@0", &[0, 0]);
        fixture.property(b"riscv,isa-base", b"rv64i\0");
        fixture.property(b"riscv,isa-extensions", modern);
        fixture.property(b"riscv,isa", legacy);
        fixture.end();
        fixture.end();
        let bytes = fixture.finish();
        let scope = read_scope(&bytes);
        let cpu = scope.cpu(0).unwrap().unwrap();
        RiscvIsa::describe(&cpu).map(|isa| isa.map(|isa| isa.has(RiscvExtension::SupervisorTimer)))
    });
    assert_eq!(results, [Err(Error::Conflict), Err(Error::Conflict)]);
}

#[test]
fn preserves_sparse_hart_ids_and_allocates_no_identity_from_description() {
    let mut fixture = cpu_scope();
    fixture.cells(b"timebase-frequency", &[10_000_000]);
    start_cpu(&mut fixture, b"cpu@0", &[0, 0]);
    fixture.end();
    start_cpu(&mut fixture, b"cpu@190000001", &[1, 0x9000_0001]);
    fixture.property(b"status", b"disabled\0");
    fixture.property(b"mmu-type", b"riscv,sv48\0");
    fixture.property(b"riscv,isa-base", b"rv64i\0");
    fixture.property(b"riscv,isa-extensions", b"i\0m\0a\0v\0sstc\0xunknown\0");
    fixture.end();
    fixture.end();
    let bytes = fixture.finish();
    let scope = read_scope(&bytes);
    let sparse = 0x1_9000_0001;
    let cpu = scope.cpu(sparse).unwrap().unwrap();
    assert_eq!(cpu.id(), sparse);
    assert!(!cpu.enabled());
    assert!(scope.cpu(0).unwrap().unwrap().enabled());
    assert!(scope.cpu(sparse as u32 as u64).unwrap().is_none());
    let mmu = RiscvMmu::describe(&cpu).unwrap().unwrap();
    assert_eq!(mmu.maximum(), MaximumMode::Paged(Mode::Sv48));
    assert_eq!(mmu.source(), cpu.node().id());
    let isa = RiscvIsa::describe(&cpu).unwrap().unwrap();
    assert!(isa.has(RiscvExtension::Vector));
    assert!(isa.has(RiscvExtension::SupervisorTimer));
    assert!(!isa.has(RiscvExtension::Float));
    assert_eq!(isa.source(), cpu.node().id());
    let timebase = RiscvTimebase::describe(&scope).unwrap().unwrap();
    assert_eq!(timebase.hertz(), 10_000_000);
    assert_eq!(timebase.source(), scope.node().id());
}

#[test]
fn rejects_unrelated_duplicate_harts_and_malformed_hart_registers() {
    for (reg, result) in [
        ([0, 1, 0].as_slice(), Error::Format),
        ([0, 1].as_slice(), Error::Conflict),
    ] {
        let mut fixture = cpu_scope();
        start_cpu(&mut fixture, b"cpu@0", &[0, 0]);
        fixture.end();
        start_cpu(&mut fixture, b"cpu@1", &[0, 1]);
        fixture.end();
        start_cpu(&mut fixture, b"cpu@2", reg);
        fixture.end();
        fixture.end();
        let bytes = fixture.finish();
        assert!(matches!(read_scope(&bytes).cpu(0), Err(error) if error == result));
    }
}

#[test]
fn distinguishes_absent_hart_capabilities_from_invalid_and_conflicting_values() {
    let mut fixture = cpu_scope();
    fixture.cells(b"timebase-frequency", &[0]);
    start_cpu(&mut fixture, b"cpu@0", &[0, 0]);
    fixture.end();
    start_cpu(&mut fixture, b"cpu@1", &[0, 1]);
    fixture.property(b"mmu-type", b"riscv,sv39");
    fixture.property(b"riscv,isa-base", b"rv64i\0");
    fixture.property(b"riscv,isa-extensions", b"i\0m\0a\0v\0");
    fixture.property(b"riscv,isa", b"rv64ima\0");
    fixture.end();
    start_cpu(&mut fixture, b"cpu@2", &[0, 2]);
    fixture.property(b"mmu-type", b"riscv,future\0");
    fixture.property(b"riscv,isa", b"rv64g2p0cv_sstc2p0\0");
    fixture.end();
    start_cpu(&mut fixture, b"cpu@3", &[0, 3]);
    fixture.property(b"riscv,isa-base", b"rv64i\0");
    fixture.property(b"riscv,isa-extensions", b"i\0xunknown\0xunknown\0");
    fixture.end();
    start_cpu(&mut fixture, b"cpu@4", &[0, 4]);
    fixture.property(b"riscv,isa-base", b"rv64i\0");
    fixture.end();
    start_cpu(&mut fixture, b"cpu@5", &[0, 5]);
    fixture.property(b"riscv,isa", b"rv64imafdc\0");
    fixture.end();
    start_cpu(&mut fixture, b"cpu@6", &[0, 6]);
    fixture.property(b"riscv,isa-base", b"rv64i\0");
    fixture.property(b"riscv,isa-extensions", b"i\0");
    fixture.end();
    start_cpu(&mut fixture, b"cpu@7", &[0, 7]);
    fixture.property(b"riscv,isa", b"rv64imafdcsstc2p0\0");
    fixture.end();
    fixture.end();
    let bytes = fixture.finish();
    let scope = read_scope(&bytes);
    let only_hart = scope.cpu(0).unwrap().unwrap();
    assert!(RiscvMmu::describe(&only_hart).unwrap().is_none());
    assert!(RiscvIsa::describe(&only_hart).unwrap().is_none());
    assert!(matches!(RiscvTimebase::describe(&scope), Err(Error::Range)));
    let conflicting = scope.cpu(1).unwrap().unwrap();
    assert!(matches!(
        RiscvMmu::describe(&conflicting),
        Err(Error::Format)
    ));
    assert!(matches!(
        RiscvIsa::describe(&conflicting),
        Err(Error::Conflict)
    ));
    let legacy = scope.cpu(2).unwrap().unwrap();
    assert!(matches!(
        RiscvMmu::describe(&legacy),
        Err(Error::Unsupported)
    ));
    let isa = RiscvIsa::describe(&legacy).unwrap().unwrap();
    assert!(isa.has(RiscvExtension::Float));
    assert!(isa.has(RiscvExtension::Csr));
    assert!(isa.has(RiscvExtension::Vector));
    assert!(isa.has(RiscvExtension::SupervisorTimer));
    assert!(matches!(
        RiscvIsa::describe(&scope.cpu(3).unwrap().unwrap()),
        Err(Error::Conflict)
    ));
    assert!(matches!(
        RiscvIsa::describe(&scope.cpu(4).unwrap().unwrap()),
        Err(Error::Missing)
    ));
    let legacy = RiscvIsa::describe(&scope.cpu(5).unwrap().unwrap())
        .unwrap()
        .unwrap();
    assert!(legacy.has(RiscvExtension::Csr));
    assert!(legacy.has(RiscvExtension::InstructionFence));
    let modern = RiscvIsa::describe(&scope.cpu(6).unwrap().unwrap())
        .unwrap()
        .unwrap();
    assert!(!modern.has(RiscvExtension::Csr));
    assert!(!modern.has(RiscvExtension::InstructionFence));
    let legacy = RiscvIsa::describe(&scope.cpu(7).unwrap().unwrap())
        .unwrap()
        .unwrap();
    assert!(legacy.has(RiscvExtension::Compressed));
    assert!(legacy.has(RiscvExtension::SupervisorTimer));
}
