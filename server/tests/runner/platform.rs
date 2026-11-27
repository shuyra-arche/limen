//! Read the test's selected platform before starting QEMU.
use kernel_verification::{Result, require};
use object::{Architecture, BinaryFormat, Object, ObjectKind, ObjectSection};
use std::{fs, path::Path};

#[path = "../target/support/platform.rs"]
mod declaration;
pub use declaration::{Board, Firmware, Interrupt, Platform, Setting};

#[derive(Debug)]
pub struct Machine {
    pub architecture: &'static str,
    pub platform: Platform,
    pub board: String,
    pub cpu: Option<&'static str>,
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
        let (architecture, board, cpu) = match (
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
                Firmware::Aavmf,
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
                Firmware::Ovmf,
                Interrupt::Apic,
            ) if !platform.aclint && platform.sstc == Setting::Default => (
                "x86_64",
                "q35".into(),
                None,
            ),
            _ => return Err(format!("unsupported image/platform combination: {platform:?}").into()),
        };
        Ok(Self {
            architecture,
            platform,
            board,
            cpu,
        })
    }
}

pub fn read(image: &Path) -> Result<Machine> {
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
        platform = Some(Platform::decode(section.data()?).ok_or("invalid test platform record")?);
    }
    Machine::admit(
        file.architecture(),
        file.format(),
        platform.ok_or("missing test platform section")?,
    )
}
