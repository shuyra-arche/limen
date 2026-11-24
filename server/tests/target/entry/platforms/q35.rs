use core::arch::asm;
use core::sync::atomic::{AtomicUsize, Ordering};
use kernel::entry::boot;
use uefi::{Status, mem::memory_map::MemoryMap};

static OLD_IDT: AtomicUsize = AtomicUsize::new(0);

pub(super) fn hart() -> usize {
    (core::arch::x86_64::__cpuid(1).ebx >> 24) as usize
}

use core::{
    ptr::NonNull,
    sync::atomic::{AtomicPtr, AtomicU64},
    time::Duration,
};
use uefi::proto::pi::mp::MpServices;

#[path = "io.rs"]
mod io;

static TRAMPOLINE: AtomicPtr<u8> = AtomicPtr::new(core::ptr::null_mut());
static TICKS_PER_US: AtomicU64 = AtomicU64::new(0);

pub(super) static SERVICES: boot::Services = boot::Services {
    output: Some(|byte| {
        unsafe { io::write8(0xe9, byte) };
        true
    }),
    reset: Some(|success| unsafe { io::write32(0xf4, if success { 0x10 } else { 0x11 }) }),
};

fn tsc() -> u64 {
    use core::arch::x86_64::{_mm_lfence, _rdtsc};
    unsafe {
        _mm_lfence();
        _rdtsc()
    }
}

fn delay(microseconds: u32) -> bool {
    let Some(ticks) = TICKS_PER_US
        .load(Ordering::Acquire)
        .checked_mul(u64::from(microseconds))
    else {
        return false;
    };
    let start = tsc();
    while tsc().wrapping_sub(start) < ticks {
        core::hint::spin_loop();
    }
    ticks != 0
}

pub(super) fn startup() -> boot::Startup {
    boot::Startup::Xapic {
        trampoline: NonNull::new(TRAMPOLINE.load(Ordering::Acquire))
            .unwrap()
            .cast(),
        delay,
    }
}

pub(super) fn capture() -> Result<Option<&'static [u8]>, Status> {
    let handle = uefi::boot::get_handle_for_protocol::<MpServices>().map_err(|e| e.status())?;
    let mp = uefi::boot::open_protocol_exclusive::<MpServices>(handle).map_err(|e| e.status())?;
    let count = mp.get_number_of_processors().map_err(|e| e.status())?;
    let mut target = None;
    for index in 0..count.total {
        let cpu = mp.get_processor_info(index).map_err(|e| e.status())?;
        if !cpu.is_bsp() && cpu.is_healthy() && cpu.is_enabled() && cpu.processor_id < 255 {
            target = Some(cpu.processor_id as usize);
            break;
        }
    }
    crate::TARGET.store(target.ok_or(Status::NOT_FOUND)?, Ordering::Release);
    // Stall is available until Boot Services exit. Save a TSC calibration
    // for the subsequent INIT/SIPI delays on this same major CPU.
    let start = tsc();
    uefi::boot::stall(Duration::from_millis(10));
    let ticks = tsc()
        .checked_sub(start)
        .filter(|v| *v != 0)
        .ok_or(Status::DEVICE_ERROR)?;
    TICKS_PER_US.store(ticks.div_ceil(10_000), Ordering::Release);
    let page = uefi::boot::allocate_pages(
        uefi::boot::AllocateType::MaxAddress(0xfffff),
        uefi::boot::MemoryType::LOADER_CODE,
        1,
    )
    .map_err(|e| e.status())?;
    TRAMPOLINE.store(page.as_ptr(), Ordering::Release);
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
    // Confirm the active table consists of the kernel's diagnostic gates.
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
