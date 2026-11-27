pub(crate) unsafe fn write_byte(base: usize, byte: u8) -> bool {
    let uart = core::ptr::without_provenance_mut::<u32>(base);
    // Avoid blocking failure shutdown forever on a stalled UART.
    for _ in 0..65536 {
        if unsafe { uart.wrapping_add(6).read_volatile() } & (1 << 5) == 0 {
            unsafe { uart.write_volatile(u32::from(byte)) };
            return true;
        }
    }
    false
}
