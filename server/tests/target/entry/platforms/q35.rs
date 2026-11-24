use core::arch::asm;
use kernel::entry::boot;
use uefi::{
    mem::memory_map::{MemoryMap, MemoryMapOwned},
    prelude::*,
};

struct Handoff {
    map: MemoryMapOwned,
    previous_idt: usize,
}

#[entry]
fn main() -> Status {
    let map = unsafe { uefi::boot::exit_boot_services(None) };
    let handoff = Handoff {
        map,
        previous_idt: Idtr::read().base,
    };
    // The old stack and final firmware map remain reserved and readable.
    unsafe { boot::enter((&raw const handoff) as usize, inspect) }
}

#[repr(C, packed)]
struct Idtr {
    limit: u16,
    base: usize,
}

impl Idtr {
    fn read() -> Self {
        let mut value = Self { limit: 0, base: 0 };
        unsafe { asm!("sidt [{}]", in(reg) &mut value, options(nostack, preserves_flags)) };
        value
    }
}

extern "sysv64" fn inspect(input: usize) -> ! {
    let handoff = unsafe { &*(input as *const Handoff) };
    let idtr = Idtr::read();
    let (sp, flags, cr0): (usize, usize, usize);
    let cs: u16;
    unsafe {
        asm!("mov {}, rsp", "pushfq", "pop {}", "mov {}, cr0", "mov {:x}, cs",
             out(reg) sp, out(reg) flags, out(reg) cr0, out(reg) cs);
    }
    assert!(
        handoff.map.entries().any(|entry| entry.page_count > 0)
            && cs & 3 == 0
            && cr0 & (1 << 31) != 0
    );
    assert!(flags & (1 << 9) == 0 && boot::stack_range().contains(&sp));
    assert!(idtr.limit == 4095 && idtr.base != 0 && idtr.base != handoff.previous_idt);
    // Confirm the active table consists of the kernel's fail-stop gates.
    for i in 0..256 {
        let gate = (idtr.base + 16 * i) as *const u8;
        assert!(unsafe { gate.add(5).read_volatile() } == 0x8e);
        assert!(unsafe { gate.add(2).cast::<u16>().read_unaligned() } == cs);
    }
    use core::fmt::Write;
    boot::console()
        .expect("board console")
        .write_str("PASS q35 ebs=1 cpl0=1 paging=1 masked=1 stack=1 idt=1\n")
        .expect("diagnostic output");
    boot::shutdown(true)
}
