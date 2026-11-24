use core::arch::asm;
use kernel::entry::boot;
use uefi::{
    mem::memory_map::{MemoryMap, MemoryMapOwned},
    prelude::*,
};

#[entry]
fn main() -> Status {
    let map = unsafe { uefi::boot::exit_boot_services(None) };
    // The old stack and final firmware map remain reserved and readable.
    unsafe { boot::enter((&raw const map) as usize, inspect) }
}

#[repr(C, packed)]
struct Idtr {
    limit: u16,
    base: usize,
}

extern "sysv64" fn inspect(input: usize) -> ! {
    let map = unsafe { &*(input as *const MemoryMapOwned) };
    let mut idtr = Idtr { limit: 0, base: 0 };
    let (sp, flags, cr0): (usize, usize, usize);
    let cs: u16;
    unsafe {
        asm!("sidt [{}]", in(reg) &mut idtr, options(nostack, preserves_flags));
        asm!("mov {}, rsp", "pushfq", "pop {}", "mov {}, cr0", "mov {:x}, cs",
             out(reg) sp, out(reg) flags, out(reg) cr0, out(reg) cs);
    }
    assert!(map.entries().any(|entry| entry.page_count > 0) && cs & 3 == 0 && cr0 & (1 << 31) != 0);
    assert!(flags & (1 << 9) == 0 && boot::stack_range().contains(&sp));
    assert!(idtr.limit == 4095 && idtr.base != 0);
    // Confirm the active table consists of the kernel's fail-stop gates.
    for i in 0..256 {
        let gate = (idtr.base + 16 * i) as *const u8;
        assert!(unsafe { gate.add(5).read_volatile() } == 0x8e);
        assert!(unsafe { gate.add(2).cast::<u16>().read_unaligned() } == cs);
    }
    for &byte in b"PASS q35 ebs=1 cpl0=1 paging=1 masked=1 stack=1 idt=1\n" {
        unsafe { asm!("out dx, al", in("dx") 0xe9u16, in("al") byte) };
    }
    unsafe { asm!("out dx, eax", in("dx") 0xf4u16, in("eax") 0x10u32) };
    loop {
        core::hint::spin_loop();
    }
}
