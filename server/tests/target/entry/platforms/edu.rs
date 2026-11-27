//! One QEMU EDU endpoint at the PCI function wired by the q35 launcher.
use core::arch::{asm, x86_64::_mm_mfence};
use core::sync::atomic::Ordering;
use kernel::metal::memory::mmu::internal::device::DeviceWindow;
use limen_treaty::model::{ByteLength, InternalAddress, PhysicalAddress};

pub(super) const REQUESTER: u16 = 6 << 3;
const EDU_BUFFER: u32 = 0x40000;
const DMA_MASK: u32 = 0x0fff_ffff;

fn select(device: u16, offset: u16) {
    unsafe {
        super::io::write32(
            0xcf8,
            0x8000_0000 | (u32::from(device) << 8) | u32::from(offset),
        )
    };
}

pub(super) fn config(device: u16, offset: u16) -> u32 {
    select(device, offset);
    let value;
    unsafe { asm!("in eax, dx", in("dx") 0xcfcu16, out("eax") value, options(nostack)) };
    value
}

pub(super) struct Endpoint(DeviceWindow);

impl Endpoint {
    pub(super) fn open() -> Self {
        assert_eq!(config(REQUESTER, 0), 0x11e8_1234);
        let bar = config(REQUESTER, 0x10);
        assert_eq!(bar & 15, 0, "EDU requires a non-prefetchable 32-bit BAR");
        assert!(bar >= 128 * 1024 * 1024 && bar.is_multiple_of(1024 * 1024));
        let command = config(REQUESTER, 4) as u16 | 6;
        // Address selection is still for the command register. Only change the low
        // half, since writing the PCI status half could acknowledge unrelated bits.
        unsafe { asm!("out dx, ax", in("dx") 0xcfcu16, in("ax") command, options(nostack)) };
        assert_eq!(config(REQUESTER, 4) & 6, 6);
        let bar = u64::from(bar);
        let range = PhysicalAddress::new(bar)
            .range(ByteLength::new(0x10_0000))
            .unwrap();
        // The retained q35 identity map covers the BAR assigned by OVMF. The only
        // owner of the endpoint after ExitBootServices is this single major CPU.
        let mut registers =
            unsafe { DeviceWindow::admit(range, InternalAddress::new(bar)) }.unwrap();
        assert_eq!(unsafe { registers.read_u32(0) }.unwrap() & 0xffff, 0xed);
        Self(registers)
    }

    /// The caller retains initialized source storage with exclusive DMA ownership
    /// until the device completes or the guest terminates.
    pub(super) unsafe fn read_from_ram(&mut self, address: u32) {
        assert!(address <= DMA_MASK - 3);
        self.transfer(address, EDU_BUFFER, 1);
    }

    /// Every possible target of this IOVA must be fixture-owned writable storage.
    /// The caller excludes CPU references until the device completes.
    pub(super) unsafe fn write_to_ram(&mut self, address: u32) {
        assert!(address <= DMA_MASK - 3);
        self.transfer(EDU_BUFFER, address, 3);
    }

    fn transfer(&mut self, source: u32, destination: u32, command: u32) {
        let edu = &mut self.0;
        unsafe {
            assert_eq!(edu.read_u32(0x98).unwrap() & 1, 0);
            edu.write_u32(0x80, source).unwrap();
            edu.write_u32(0x88, destination).unwrap();
            edu.write_u32(0x90, 4).unwrap();
            edu.write_u32(0x98, command).unwrap();
        }
        let start = super::tsc();
        let budget = super::TICKS_PER_US
            .load(Ordering::Acquire)
            .checked_mul(2_000_000)
            .unwrap();
        while unsafe { edu.read_u32(0x98) }.unwrap() & 1 != 0 {
            assert!(
                super::tsc().wrapping_sub(start) < budget,
                "EDU completion timed out"
            );
            core::hint::spin_loop();
        }
        unsafe { _mm_mfence() };
    }
}
