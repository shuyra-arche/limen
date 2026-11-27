use core::arch::asm;

pub unsafe fn write32(port: u16, value: u32) {
    unsafe { asm!("out dx, eax", in("dx") port, in("eax") value, options(nostack)) };
}

pub unsafe fn write8(port: u16, value: u8) {
    unsafe { asm!("out dx, al", in("dx") port, in("al") value, options(nostack)) };
}
