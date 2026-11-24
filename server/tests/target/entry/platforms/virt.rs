use core::arch::{asm, global_asm};
use kernel::entry::boot;

// QEMU's AArch64 Linux Image protocol passes the DTB physical address in x0.
// This entry uses no stack before transferring to the production prefix.
global_asm!(
    ".pushsection .text.header, \"ax\"",
    ".global image",
    "image: b 2f",
    ".long 0",
    ".quad 0x80000",
    ".quad __image_size",
    ".quad 2", // little endian, 4 KiB pages, legacy placement
    ".quad 0, 0, 0",
    ".ascii \"ARM\\x64\"",
    ".long 0",
    "2: adrp x1, {inspect}",
    "add x1, x1, :lo12:{inspect}",
    "b {enter}",
    ".popsection",
    inspect = sym inspect,
    enter = sym boot::enter,
);

extern "C" fn inspect(dtb: usize) -> ! {
    let (el, daif, sp, vectors, spsel): (usize, usize, usize, usize, usize);
    unsafe {
        asm!("mrs {}, CurrentEL", "mrs {}, DAIF", "mov {}, sp", "mrs {}, VBAR_EL1",
             "mrs {}, SPSel", out(reg) el, out(reg) daif, out(reg) sp, out(reg) vectors,
             out(reg) spsel, options(nostack, nomem, preserves_flags));
    }
    assert!(el == 4 && daif & 0x3c0 == 0x3c0 && spsel == 1);
    assert!(boot::stack_range().contains(&sp));
    unsafe extern "C" {
        static limen_vectors: u8;
    }
    assert!(vectors == (&raw const limen_vectors) as usize && vectors & 2047 == 0);
    // Fixture-owned virt RAM aperture (-m 128M); check bounds before reading.
    let ram = 0x40000000..0x48000000;
    assert!(dtb & 7 == 0 && ram.contains(&dtb) && dtb <= ram.end - 40);
    let words = dtb as *const u32;
    let magic = u32::from_be(unsafe { words.read_volatile() });
    let size = u32::from_be(unsafe { words.add(1).read_volatile() }) as usize;
    assert!(magic == 0xd00dfeed && (40..=ram.end - dtb).contains(&size));
    let uart = 0x09000000 as *mut u32;
    for &byte in b"PASS virt el1=1 masked=1 stack=1 vectors=1 dtb=1\n" {
        while unsafe { uart.add(6).read_volatile() } & (1 << 5) != 0 {}
        unsafe { uart.write_volatile(u32::from(byte)) };
    }
    // This virt configuration supplies PSCI over HVC.
    unsafe { asm!("hvc #0", in("x0") 0x84000008usize, options(noreturn)) };
}
