#[path = "../../entry/boot/fault.rs"]
mod boot_fault;
#[path = "../platform.rs"]
mod platform;
#[allow(dead_code, reason = "the runner executable owns the live QMP connection")]
#[path = "../qmp.rs"]
mod qmp;
#[path = "../utilities.rs"]
mod utilities;

#[cfg(unix)]
#[test]
fn symbolizer_preserves_literal_return_addresses() {
    use std::os::unix::fs::PermissionsExt;
    let root = std::env::temp_dir().join(format!("kernel-symbolize-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir(&root).unwrap();
    let tool = root.join("addr2line");
    let serial = root.join("serial.log");
    std::fs::write(
        &tool,
        "#!/bin/sh\nprintf '%s\\n' \"$5\" >> \"$4.addresses\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(&serial, b"unrelated pc=0x50\nFAIL Limen boot trap: cause=0x2 pc=0x80200040 value=0x0 stack=0x80208000 status=0x100\n").unwrap();
    utilities::symbolize(&root.join("kernel.elf"), &serial, tool.to_str().unwrap()).unwrap();
    assert_eq!(
        std::fs::read_to_string(root.join("kernel.elf.addresses")).unwrap(),
        "0x80200040\n"
    );
    assert!(utilities::symbolize(&root.join("kernel.elf"), &serial, "/bin/false").is_err());
    std::fs::remove_dir_all(root).unwrap();
}

mod execution;
mod transport;

#[test]
fn platform_record_is_bounded_and_rejects_unsupported_combinations() {
    use object::{Architecture, BinaryFormat};
    use platform::{Board, Firmware, Interrupt, Iommu, Machine, Platform, Serial, Setting};

    let original = Platform {
        board: Board::Virt,
        firmware: Firmware::OpenSbi,
        interrupt: Interrupt::Aplic,
        harts: 1,
        aclint: true,
        sstc: Setting::Off,
        iommu: Iommu::None,
        serial: Serial::Default,
        devices: [None; 4],
    };
    let bytes = original.encode();
    assert_eq!(Platform::decode(&bytes), Some(original));
    assert!(Platform::decode(&bytes[..15]).is_none());
    for (index, value) in [
        (0, 0),
        (8, 1),
        (9, 0),
        (10, 0),
        (11, 0),
        (13, 2),
        (14, 3),
        (15, 5),
        (16, 3),
        (17, 1),
        (20, 2),
    ] {
        let mut invalid = bytes;
        invalid[index] = value;
        assert!(Platform::decode(&invalid).is_none());
    }

    let machine = Machine::admit(Architecture::Riscv64, BinaryFormat::Elf, original).unwrap();
    assert_eq!(machine.board, "virt,aia=aplic,aclint=on");
    assert_eq!(machine.cpu, Some("rv64,sstc=false"));
    assert_eq!(machine.platform.harts, 1);
    for invalid in [
        Platform {
            harts: 0,
            ..original
        },
        Platform {
            harts: 65,
            ..original
        },
        Platform {
            firmware: Firmware::Aavmf,
            ..original
        },
        Platform {
            interrupt: Interrupt::GicV3,
            ..original
        },
        Platform {
            board: Board::Q35,
            ..original
        },
    ] {
        assert!(Machine::admit(Architecture::Riscv64, BinaryFormat::Elf, invalid).is_err());
    }
    assert!(Machine::admit(Architecture::Riscv64, BinaryFormat::Pe, original).is_err());
    assert!(Machine::admit(Architecture::X86_64, BinaryFormat::Elf, original).is_err());
}

#[test]
fn platform_device_topology_is_composed_without_test_identity() {
    use object::{Architecture, BinaryFormat};
    use platform::{
        Board, Device, DmaMask, Firmware, Interrupt, Iommu, Machine, Platform, Serial, Setting,
    };

    let device = |slot| Device::Edu {
        slot,
        function: 0,
        dma_mask: DmaMask::Full32,
    };
    let original = Platform {
        board: Board::Virt,
        firmware: Firmware::OpenSbi,
        interrupt: Interrupt::Plic,
        harts: 1,
        aclint: false,
        sstc: Setting::Default,
        iommu: Iommu::Riscv,
        serial: Serial::Default,
        devices: [None; 4],
    }
    .device(device(6))
    .device(device(7));
    assert_eq!(Platform::decode(&original.encode()), Some(original));
    let machine = Machine::admit(Architecture::Riscv64, BinaryFormat::Elf, original).unwrap();
    assert_eq!(machine.board, "virt,aia=none,aclint=off,iommu-sys=on");
    assert_eq!(
        machine.devices,
        [
            "edu,addr=06.0,dma_mask=0xffffffff",
            "edu,addr=07.0,dma_mask=0xffffffff",
        ]
    );
    for invalid in [
        original.device(device(6)),
        original.device(device(32)),
        Platform {
            iommu: Iommu::Vtd,
            ..original
        },
        Platform {
            serial: Serial::Second,
            ..original
        },
    ] {
        assert!(Machine::admit(Architecture::Riscv64, BinaryFormat::Elf, invalid).is_err());
    }
    let mut missing = original.encode();
    missing[20..24].fill(0);
    assert!(Platform::decode(&missing).is_none());

    let arm = Platform {
        firmware: Firmware::Aavmf,
        interrupt: Interrupt::GicV3,
        harts: 4,
        iommu: Iommu::SmmuV3,
        serial: Serial::Second,
        ..original
    };
    let machine = Machine::admit(Architecture::Aarch64, BinaryFormat::Pe, arm).unwrap();
    assert!(machine.board.ends_with(",iommu=smmuv3"));
    assert!(machine.secondary_serial);

    let x86 = Platform {
        board: Board::Q35,
        firmware: Firmware::Ovmf,
        interrupt: Interrupt::Apic,
        harts: 4,
        iommu: Iommu::Vtd,
        serial: Serial::Isa,
        ..original
    };
    let machine = Machine::admit(Architecture::X86_64, BinaryFormat::Pe, x86).unwrap();
    assert_eq!(
        machine.devices,
        [
            "intel-iommu",
            "isa-serial,index=0",
            "edu,bus=pcie.0,addr=06.0,dma_mask=0xffffffff",
            "edu,bus=pcie.0,addr=07.0,dma_mask=0xffffffff",
        ]
    );
}

#[test]
fn strict_nx_firmware_is_selected_by_the_guest() {
    use object::{Architecture, BinaryFormat};
    use platform::{Board, Firmware, Interrupt, Iommu, Machine, Platform, Serial, Setting};

    let arm = Platform {
        board: Board::Virt,
        firmware: Firmware::AavmfStrictNx,
        interrupt: Interrupt::GicV3,
        harts: 1,
        aclint: false,
        sstc: Setting::Default,
        iommu: Iommu::None,
        serial: Serial::Default,
        devices: [None; 4],
    };
    assert_eq!(Platform::decode(&arm.encode()), Some(arm));
    assert!(Machine::admit(Architecture::Aarch64, BinaryFormat::Pe, arm).is_ok());
    assert!(Machine::admit(Architecture::Riscv64, BinaryFormat::Elf, arm).is_err());
    let x86 = Platform {
        board: Board::Q35,
        firmware: Firmware::OvmfStrictNx,
        interrupt: Interrupt::Apic,
        ..arm
    };
    assert_eq!(Platform::decode(&x86.encode()), Some(x86));
    let machine = Machine::admit(Architecture::X86_64, BinaryFormat::Pe, x86).unwrap();
    assert_eq!(machine.board, "q35,smm=on");
    assert!(Machine::admit(Architecture::Aarch64, BinaryFormat::Pe, x86).is_err());
}
