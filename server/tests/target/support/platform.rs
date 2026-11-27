//! Test-owned platform description, carried in the linked executable.
//! The record is data, not a request to select a kernel implementation.
#![allow(dead_code, reason = "the record is shared by guest writers and the host reader")]

pub const SECTION: &[u8] = b".testenv";
pub const SIZE: usize = 16;

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
pub struct Platform {
    pub board: Board,
    pub firmware: Firmware,
    pub interrupt: Interrupt,
    pub harts: u8,
    pub aclint: bool,
    pub sstc: Setting,
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
        }
    }

    pub const fn encode(self) -> [u8; SIZE] {
        [
            b'L',
            b'I',
            b'M',
            b'E',
            b'N',
            b'P',
            b'L',
            b'T',
            1,
            self.board as u8,
            self.firmware as u8,
            self.interrupt as u8,
            self.harts,
            self.aclint as u8,
            self.sstc as u8,
            0,
        ]
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let [b'L', b'I', b'M', b'E', b'N', b'P', b'L', b'T', 1, board, firmware, interrupt, harts, aclint, sstc, 0] =
            <[u8; SIZE]>::try_from(bytes).ok()?
        else {
            return None;
        };
        Some(Self {
            board: match board {
                1 => Board::Virt,
                2 => Board::Q35,
                _ => return None,
            },
            firmware: match firmware {
                1 => Firmware::OpenSbi,
                2 => Firmware::Aavmf,
                3 => Firmware::Ovmf,
                _ => return None,
            },
            interrupt: match interrupt {
                1 => Interrupt::Plic,
                2 => Interrupt::Aplic,
                3 => Interrupt::GicV3,
                4 => Interrupt::Apic,
                _ => return None,
            },
            harts,
            aclint: match aclint {
                0 => false,
                1 => true,
                _ => return None,
            },
            sstc: match sstc {
                0 => Setting::Default,
                1 => Setting::On,
                2 => Setting::Off,
                _ => return None,
            },
        })
    }
}

#[cfg(any(target_os = "none", target_os = "uefi"))]
macro_rules! declare {
    ($platform:expr) => {
        #[used]
        #[unsafe(no_mangle)]
        #[unsafe(link_section = ".testenv")]
        static LIMEN_TEST_PLATFORM: [u8; $crate::platform::SIZE] = ($platform).encode();
    };
}

#[cfg(any(target_os = "none", target_os = "uefi"))]
pub(super) use declare;
