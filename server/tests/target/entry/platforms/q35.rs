use core::arch::asm;
use core::sync::atomic::{AtomicUsize, Ordering};
use kernel::entry::boot;
use uefi::{Status, mem::memory_map::MemoryMap};

static OLD_IDT: AtomicUsize = AtomicUsize::new(0);

pub(super) fn hart() -> usize {
    (core::arch::x86_64::__cpuid(1).ebx >> 24) as usize
}

pub(super) fn capture() -> Result<Option<&'static [u8]>, Status> {
    let page = uefi::boot::allocate_pages(
        uefi::boot::AllocateType::MaxAddress(0xfffff),
        uefi::boot::MemoryType::LOADER_CODE,
        1,
    )
    .map_err(|e| e.status())?;
    unsafe {
        crate::MINOR
            .reserve_trampoline(&mut *page.cast::<[core::mem::MaybeUninit<u8>; 4096]>().as_ptr())
    }
    .map_err(|_| Status::INVALID_PARAMETER)?;
    OLD_IDT.store(Idtr::read().base, Ordering::Relaxed);
    Ok(None)
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

pub(super) fn inspect(boot: boot::Boot, _: &impl MemoryMap) -> ! {
    assert!(boot.device_tree().is_none());
    let idtr = Idtr::read();
    let (sp, flags, cr0): (usize, usize, usize);
    let cs: u16;
    unsafe {
        asm!("mov {}, rsp", "pushfq", "pop {}", "mov {}, cr0", "mov {:x}, cs",
             out(reg) sp, out(reg) flags, out(reg) cr0, out(reg) cs);
    }
    assert!(cs & 3 == 0 && cr0 & (1 << 31) != 0);
    assert!(flags & (1 << 9) == 0 && boot::stack_range().contains(&sp));
    assert!(idtr.limit == 4095 && idtr.base != 0 && idtr.base != OLD_IDT.load(Ordering::Relaxed));
    // Confirm the active table consists of the kernel's fail-stop gates.
    for i in 0..256 {
        let gate = (idtr.base + 16 * i) as *const u8;
        assert!(unsafe { gate.add(5).read_volatile() } == 0x8e);
        assert!(unsafe { gate.add(2).cast::<u16>().read_unaligned() } == cs);
    }
    use core::fmt::Write;
    boot::console()
        .expect("board console")
        .write_str("PASS q35 major=1 minor=1 ebs=1 cpl0=1 paging=1 masked=1 stack=1 idt=1\n")
        .expect("diagnostic output");
    boot::shutdown(true)
}
