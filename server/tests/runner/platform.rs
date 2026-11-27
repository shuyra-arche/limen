//! Read the test's selected platform before starting QEMU.
use crate::{Result, require};
use object::{Architecture, BinaryFormat, Object, ObjectKind, ObjectSection};
use std::{fs, path::Path};

#[path = "../target/support/platform.rs"]
mod declaration;
pub use declaration::{
    Board, Declaration, Device, DmaMask, Firmware, Interrupt, Iommu, Platform, Purpose, Serial,
    Setting,
};

#[derive(Debug)]
pub struct Machine {
    pub architecture: &'static str,
    pub platform: Platform,
    pub board: String,
    pub cpu: Option<&'static str>,
    pub devices: Vec<String>,
    pub secondary_serial: bool,
}

impl Machine {
    pub fn admit(
        architecture: Architecture,
        format: BinaryFormat,
        platform: Platform,
    ) -> Result<Self> {
        require(
            (1..=64).contains(&platform.harts),
            "invalid test Hart count",
        )?;
        let (architecture, mut board, cpu) = match (
            architecture,
            format,
            platform.board,
            platform.firmware,
            platform.interrupt,
        ) {
            (
                Architecture::Riscv64,
                BinaryFormat::Elf,
                Board::Virt,
                Firmware::OpenSbi,
                Interrupt::Plic | Interrupt::Aplic,
            ) => {
                let aia = match platform.interrupt {
                    Interrupt::Plic => "none",
                    Interrupt::Aplic => "aplic",
                    _ => unreachable!(),
                };
                let aclint = if platform.aclint { "on" } else { "off" };
                let cpu = match platform.sstc {
                    Setting::Default => "rv64",
                    Setting::On => "rv64,sstc=true",
                    Setting::Off => "rv64,sstc=false",
                };
                (
                    "riscv64",
                    format!("virt,aia={aia},aclint={aclint}"),
                    Some(cpu),
                )
            }
            (
                Architecture::Aarch64,
                BinaryFormat::Pe,
                Board::Virt,
                Firmware::Aavmf | Firmware::AavmfStrictNx,
                Interrupt::GicV3,
            ) if !platform.aclint && platform.sstc == Setting::Default => (
                "aarch64",
                "virt,gic-version=3,secure=off,virtualization=off,acpi=off".into(),
                Some("cortex-a57"),
            ),
            (
                Architecture::X86_64,
                BinaryFormat::Pe,
                Board::Q35,
                Firmware::Ovmf | Firmware::OvmfStrictNx,
                Interrupt::Apic,
            ) if !platform.aclint && platform.sstc == Setting::Default => {
                let board = match platform.firmware {
                    Firmware::OvmfStrictNx => "q35,smm=on",
                    _ => "q35",
                };
                ("x86_64", board.into(), None)
            }
            _ => {
                return Err(format!("unsupported image/platform combination: {platform:?}").into());
            }
        };
        let mut devices = Vec::new();
        match (architecture, platform.iommu) {
            (_, Iommu::None) => {}
            ("riscv64", Iommu::Riscv) => board.push_str(",iommu-sys=on"),
            ("aarch64", Iommu::SmmuV3) => board.push_str(",iommu=smmuv3"),
            ("x86_64", Iommu::AmdVi) => devices.push("amd-iommu,dma-remap=on".into()),
            ("x86_64", Iommu::Vtd) => devices.push("intel-iommu".into()),
            _ => return Err("unsupported IOMMU for this machine".into()),
        }
        let secondary_serial = match (architecture, platform.serial) {
            (_, Serial::Default) => false,
            ("aarch64", Serial::Second) => true,
            ("x86_64", Serial::Isa) => {
                devices.push("isa-serial,index=0".into());
                false
            }
            _ => return Err("unsupported serial topology for this machine".into()),
        };
        let mut occupied = [false; 32];
        let mut ended = false;
        for device in platform.devices {
            let Some(device) = device else {
                ended = true;
                continue;
            };
            require(!ended, "noncontiguous device description")?;
            match device {
                Device::Edu {
                    slot,
                    function,
                    dma_mask,
                } => {
                    // Only standalone PCI functions are presently supported.
                    require(
                        (1..32).contains(&slot) && function == 0,
                        "unsupported PCI address",
                    )?;
                    require(!occupied[usize::from(slot)], "duplicate PCI address")?;
                    occupied[usize::from(slot)] = true;
                    let bus = if architecture == "x86_64" {
                        ",bus=pcie.0"
                    } else {
                        ""
                    };
                    let mask = match dma_mask {
                        DmaMask::Default => "",
                        DmaMask::Full32 => ",dma_mask=0xffffffff",
                    };
                    devices.push(format!("edu{bus},addr={slot:02x}.{function}{mask}"));
                }
            }
        }
        Ok(Self {
            architecture,
            platform,
            board,
            cpu,
            devices,
            secondary_serial,
        })
    }
}

/// Inspect the actual compiled image, without selecting an execution purpose.
pub(crate) fn inspect(image: &Path) -> Result<(Machine, Purpose)> {
    let bytes = fs::read(image)?;
    let file = object::File::parse(bytes.as_slice())?;
    require(
        file.is_64() && file.is_little_endian() && file.kind() == ObjectKind::Executable,
        "expected a little-endian 64-bit executable",
    )?;
    let mut platform = None;
    for section in file.sections() {
        if section.name_bytes()? != declaration::SECTION {
            continue;
        }
        require(platform.is_none(), "duplicate test platform section")?;
        require(
            section.size() == declaration::SIZE as u64,
            "invalid platform section size",
        )?;
        platform = Some(Declaration::decode(section.data()?).ok_or("invalid guest declaration")?);
    }
    let declaration = platform.ok_or("missing guest declaration")?;
    let machine = Machine::admit(file.architecture(), file.format(), declaration.platform)?;
    Ok((machine, declaration.purpose))
}

pub fn read(image: &Path, purpose: Purpose) -> Result<Machine> {
    let (machine, actual) = inspect(image)?;
    require(
        actual == purpose,
        format!(
            "image purpose is {}, requested {}",
            actual.name(),
            purpose.name()
        ),
    )?;
    Ok(machine)
}
