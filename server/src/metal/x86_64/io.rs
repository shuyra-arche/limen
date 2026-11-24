use core::arch::asm;

pub(crate) unsafe fn read32(port: u16) -> u32 {
    let value;
    unsafe { asm!("in eax, dx", in("dx") port, out("eax") value, options(nostack)) };
    value
}

pub(crate) unsafe fn write32(port: u16, value: u32) {
    unsafe { asm!("out dx, eax", in("dx") port, in("eax") value, options(nostack)) };
}

pub(crate) unsafe fn write8(port: u16, value: u8) {
    unsafe { asm!("out dx, al", in("dx") port, in("al") value, options(nostack)) };
}
