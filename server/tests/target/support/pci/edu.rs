//! EDU register and completion protocol shared by the PCI board fixtures.

use kernel::metal::memory::mmu::access::device::DeviceWindow;

const SOURCE: usize = 0x80;
const DESTINATION: usize = 0x88;
const COUNT: usize = 0x90;
const COMMAND: usize = 0x98;
const START: u32 = 1;
const WRITE_TO_RAM: u32 = 2;
const BUFFER: u32 = 0x4_0000;
const BYTES: u32 = 4;

pub(super) enum Direction {
    FromRam,
    ToRam,
}

pub(super) struct Edu {
    registers: DeviceWindow,
    order: fn(),
    now: fn() -> u64,
    timeout: u64,
    mask: u32,
}

impl Edu {
    pub(super) fn admit(
        mut registers: DeviceWindow,
        order: fn(),
        now: fn() -> u64,
        hertz: u64,
        mask: u32,
    ) -> Self {
        assert_eq!(unsafe { registers.read_u32(0) }.unwrap() & 0xffff, 0xed);
        Self {
            registers,
            order,
            now,
            timeout: hertz.checked_mul(2).filter(|ticks| *ticks != 0).unwrap(),
            mask,
        }
    }

    /// The admitted board fixture retains the source/destination resources
    /// and excludes competing CPU/device accesses until completion.
    pub(super) unsafe fn transfer(&mut self, address: u32, direction: Direction) {
        assert!(address <= self.mask - (BYTES - 1));
        let (source, destination, command) = match direction {
            Direction::FromRam => (address, BUFFER, START),
            Direction::ToRam => (BUFFER, address, START | WRITE_TO_RAM),
        };
        let registers = &mut self.registers;
        unsafe {
            assert_eq!(registers.read_u32(COMMAND).unwrap() & START, 0);
            registers.write_u32(SOURCE, source).unwrap();
            registers.write_u32(DESTINATION, destination).unwrap();
            registers.write_u32(COUNT, BYTES).unwrap();
            (self.order)();
            registers.write_u32(COMMAND, command).unwrap();
            (self.order)();
            let start = (self.now)();
            while registers.read_u32(COMMAND).unwrap() & START != 0 {
                assert!(
                    (self.now)().wrapping_sub(start) < self.timeout,
                    "EDU timeout"
                );
                core::hint::spin_loop();
            }
            (self.order)();
        }
    }
}
