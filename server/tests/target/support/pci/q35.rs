//! QEMU EDU endpoints at the PCI functions wired by the q35 launcher.
use core::arch::{asm, x86_64::_mm_mfence};
use kernel::metal::memory::access::treaty::device::DeviceWindow;
use limen_treaty::model::{ByteLength, InternalAddress, PhysicalAddress};
use uefi_fixture::q35;

pub(super) const REQUESTER: u16 = 6 << 3;
#[path = "edu.rs"]
mod edu;
use edu::{Direction, Edu};
const DMA_MASK: u32 = 0x0fff_ffff;

fn select(device: u16, offset: u16) {
    unsafe {
        q35::io::write32(
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

pub(super) struct Endpoint(Edu);

impl Endpoint {
    pub(super) fn open() -> Self {
        Self::open_at(REQUESTER)
    }

    pub(super) fn open_at(requester: u16) -> Self {
        assert_eq!(config(requester, 0), 0x11e8_1234);
        let bar = config(requester, 0x10);
        assert_eq!(bar & 15, 0, "EDU requires a non-prefetchable 32-bit BAR");
        assert!(bar >= 128 * 1024 * 1024 && bar.is_multiple_of(1024 * 1024));
        let command = config(requester, 4) as u16 | 6;
        // Address selection is still for the command register. Only change the low
        // half, since writing the PCI status half could acknowledge unrelated bits.
        unsafe { asm!("out dx, ax", in("dx") 0xcfcu16, in("ax") command, options(nostack)) };
        assert_eq!(config(requester, 4) & 6, 6);
        let bar = u64::from(bar);
        let range = PhysicalAddress::new(bar)
            .range(ByteLength::new(0x10_0000))
            .unwrap();
        // The retained q35 identity map covers the BAR assigned by OVMF. The only
        // owner of the endpoint after ExitBootServices is this single major CPU.
        let registers = unsafe { DeviceWindow::admit(range, InternalAddress::new(bar)) }.unwrap();
        let hertz = q35::tsc_frequency();
        fn order() {
            unsafe { _mm_mfence() };
        }
        Self(Edu::admit(registers, order, q35::tsc, hertz, DMA_MASK))
    }

    /// The caller retains initialized source storage with exclusive DMA ownership
    /// until the device completes or the guest terminates.
    pub(super) unsafe fn read_from_ram(&mut self, address: u32) {
        unsafe { self.0.transfer(address, Direction::FromRam) };
    }

    /// Every possible target of this IOVA must be fixture-owned writable storage.
    /// The caller excludes CPU references until the device completes.
    pub(super) unsafe fn write_to_ram(&mut self, address: u32) {
        unsafe { self.0.transfer(address, Direction::ToRam) };
    }
}
