//! QEMU q35 diagnostic profile with debugcon and isa-debug-exit.
use crate::metal::console::Services;
use crate::metal::{minor::Minor, x86_64::io};

pub(in crate::entry) fn services() -> Services {
    Services {
        output: Some(write_byte),
        reset: Some(reset),
    }
}

fn write_byte(byte: u8) -> bool {
    unsafe { io::write8(0xe9, byte) };
    true
}

fn reset(success: bool) {
    let code: u32 = if success { 0x10 } else { 0x11 };
    unsafe { io::write32(0xf4, code) };
}

impl Minor {
    /// Reserve the q35 AP trampoline and select the ICH9 PM timer for INIT/SIPI delays.
    ///
    /// # Safety
    /// Call during major CPU setup with exclusive PCI configuration access.
    /// Supply one exclusively reserved, identity-mapped executable page below 1 MiB.
    pub unsafe fn reserve_trampoline(
        &self,
        page: &'static mut [core::mem::MaybeUninit<u8>; 4096],
    ) -> Result<(), isize> {
        unsafe { io::write32(0xcf8, 0x8000_f840) };
        let base = unsafe { io::read32(0xcfc) } as u16;
        if base & 1 == 0 || base & 0xff80 == 0 {
            return Err(-2);
        }
        self.configure_trampoline(page, (base & 0xff80) + 8)
    }
}
