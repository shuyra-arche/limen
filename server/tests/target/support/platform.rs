//! Test-owned platform description, carried in the linked executable.
//! The record is data, not a request to select a kernel implementation.
#![allow(
    dead_code,
    reason = "the record is shared by guest writers and the host reader"
)]

pub const SECTION: &[u8] = b".testenv";
const HEADER: usize = 20;
const DEVICE_SIZE: usize = 4;
pub const DEVICE_LIMIT: usize = 4;
pub const SIZE: usize = HEADER + DEVICE_SIZE * DEVICE_LIMIT;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Board {
    Virt = 1,
    Q35 = 2,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Firmware {
    OpenSbi = 1,
    Aavmf = 2,
    Ovmf = 3,
    AavmfStrictNx = 4,
    OvmfStrictNx = 5,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Interrupt {
    Plic = 1,
    Aplic = 2,
    GicV3 = 3,
    Apic = 4,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Setting {
    Default = 0,
    On = 1,
    Off = 2,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Iommu {
    None = 0,
    Riscv = 1,
    SmmuV3 = 2,
    AmdVi = 3,
    Vtd = 4,
}

/// Additional serial hardware, independent of the diagnostic transport.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Serial {
    Default = 0,
    Second = 1,
    Isa = 2,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum DmaMask {
    Default = 0,
    Full32 = 1,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Device {
    Edu {
        slot: u8,
        function: u8,
        dma_mask: DmaMask,
    },
}

impl Device {
    const fn encode(self) -> [u8; DEVICE_SIZE] {
        match self {
            Self::Edu {
                slot,
                function,
                dma_mask,
            } => [1, slot, function, dma_mask as u8],
        }
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        match bytes {
            [1, slot, function, mask] => Some(Self::Edu {
                slot: *slot,
                function: *function,
                dma_mask: match mask {
                    0 => DmaMask::Default,
                    1 => DmaMask::Full32,
                    _ => return None,
                },
            }),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Platform {
    pub board: Board,
    pub firmware: Firmware,
    pub interrupt: Interrupt,
    pub harts: u8,
    pub aclint: bool,
    pub sstc: Setting,
    pub iommu: Iommu,
    pub serial: Serial,
    pub devices: [Option<Device>; DEVICE_LIMIT],
}

impl Platform {
    #[cfg(any(target_os = "none", target_os = "uefi"))]
    pub const fn native(harts: u8) -> Self {
        #[cfg(target_arch = "riscv64")]
        let (board, firmware, interrupt) = (Board::Virt, Firmware::OpenSbi, Interrupt::Plic);
        #[cfg(target_arch = "aarch64")]
        let (board, firmware, interrupt) = (Board::Virt, Firmware::Aavmf, Interrupt::GicV3);
        #[cfg(target_arch = "x86_64")]
        let (board, firmware, interrupt) = (Board::Q35, Firmware::Ovmf, Interrupt::Apic);
        Self {
            board,
            firmware,
            interrupt,
            harts,
            aclint: false,
            sstc: Setting::Default,
            iommu: Iommu::None,
            serial: Serial::Default,
            devices: [None; DEVICE_LIMIT],
        }
    }

    pub const fn device(mut self, device: Device) -> Self {
        let mut index = 0;
        while index < DEVICE_LIMIT {
            if self.devices[index].is_none() {
                self.devices[index] = Some(device);
                return self;
            }
            index += 1;
        }
        panic!("test platform device capacity exceeded");
    }

    pub const fn encode(self) -> [u8; SIZE] {
        let header = [
            b'L',
            b'I',
            b'M',
            b'E',
            b'N',
            b'P',
            b'L',
            b'T',
            2,
            self.board as u8,
            self.firmware as u8,
            self.interrupt as u8,
            self.harts,
            self.aclint as u8,
            self.sstc as u8,
            self.iommu as u8,
            self.serial as u8,
            0,
            0,
            0,
        ];
        let mut bytes = [0; SIZE];
        let mut index = 0;
        while index < HEADER {
            bytes[index] = header[index];
            index += 1;
        }
        let mut index = 0;
        while index < DEVICE_LIMIT {
            if let Some(device) = self.devices[index] {
                let record = device.encode();
                let start = HEADER + index * DEVICE_SIZE;
                let mut offset = 0;
                while offset < DEVICE_SIZE {
                    bytes[start + offset] = record[offset];
                    offset += 1;
                }
            }
            index += 1;
        }
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let bytes: &[u8; SIZE] = bytes.try_into().ok()?;
        if &bytes[..8] != b"LIMENPLT" || bytes[8] != 2 || bytes[17..HEADER] != [0; 3] {
            return None;
        }
        let mut devices = [None; DEVICE_LIMIT];
        let mut ended = false;
        let (records, remainder) = bytes[HEADER..].as_chunks::<DEVICE_SIZE>();
        if !remainder.is_empty() {
            return None;
        }
        for (index, record) in records.iter().enumerate() {
            if *record == [0; DEVICE_SIZE] {
                ended = true;
                continue;
            }
            if ended {
                return None;
            }
            devices[index] = Some(Device::decode(record)?);
        }
        Some(Self {
            board: match bytes[9] {
                1 => Board::Virt,
                2 => Board::Q35,
                _ => return None,
            },
            firmware: match bytes[10] {
                1 => Firmware::OpenSbi,
                2 => Firmware::Aavmf,
                3 => Firmware::Ovmf,
                4 => Firmware::AavmfStrictNx,
                5 => Firmware::OvmfStrictNx,
                _ => return None,
            },
            interrupt: match bytes[11] {
                1 => Interrupt::Plic,
                2 => Interrupt::Aplic,
                3 => Interrupt::GicV3,
                4 => Interrupt::Apic,
                _ => return None,
            },
            harts: bytes[12],
            aclint: match bytes[13] {
                0 => false,
                1 => true,
                _ => return None,
            },
            sstc: match bytes[14] {
                0 => Setting::Default,
                1 => Setting::On,
                2 => Setting::Off,
                _ => return None,
            },
            iommu: match bytes[15] {
                0 => Iommu::None,
                1 => Iommu::Riscv,
                2 => Iommu::SmmuV3,
                3 => Iommu::AmdVi,
                4 => Iommu::Vtd,
                _ => return None,
            },
            serial: match bytes[16] {
                0 => Serial::Default,
                1 => Serial::Second,
                2 => Serial::Isa,
                _ => return None,
            },
            devices,
        })
    }
}

#[cfg(any(target_os = "none", target_os = "uefi"))]
#[macro_export]
macro_rules! declare {
    ($platform:expr) => {
        #[used]
        #[unsafe(no_mangle)]
        #[unsafe(link_section = ".testenv")]
        static LIMEN_TEST_PLATFORM: [u8; $crate::platform::SIZE] = ($platform).encode();
    };
}
