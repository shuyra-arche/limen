use core::arch::asm;
use core::sync::atomic::AtomicUsize;
use core::sync::atomic::Ordering;
use kernel::entry::boot;
use kernel::metal::{
    interrupt::x86_64::internal::apic::{self, LocalApic, Probe},
    memory::mmu::access::device::DeviceWindow,
};
use limen_treaty::model::PhysicalAddress;
use limen_treaty::model::{ByteLength, InternalAddress};
use uefi::Status;
use uefi::mem::memory_map::MemoryMap;

static OLD_IDT: AtomicUsize = AtomicUsize::new(0);

pub fn hart() -> usize {
    (core::arch::x86_64::__cpuid(1).ebx >> 24) as usize
}

use core::{ptr::NonNull, sync::atomic::AtomicPtr};
use core::{sync::atomic::AtomicU64, time::Duration};
use uefi::proto::pi::mp::MpServices;

#[path = "io.rs"]
pub mod io;

static TRAMPOLINE: AtomicPtr<u8> = AtomicPtr::new(core::ptr::null_mut());
static TICKS_PER_US: AtomicU64 = AtomicU64::new(0);

pub fn tsc_frequency() -> u64 {
    TICKS_PER_US
        .load(Ordering::Acquire)
        .checked_mul(1_000_000)
        .unwrap()
}

pub(super) static SERVICES: boot::Services = boot::Services {
    output: Some(|byte| {
        unsafe { io::write8(0xe9, byte) };
        true
    }),
    reset: Some(|success| unsafe { io::write32(0xf4, if success { 0x10 } else { 0x11 }) }),
};

pub fn tsc() -> u64 {
    use core::arch::x86_64::{_mm_lfence, _rdtsc};
    unsafe {
        _mm_lfence();
        _rdtsc()
    }
}

pub fn delay(microseconds: u32) -> bool {
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

pub fn local_window(boot: &boot::Boot<'_>) -> DeviceWindow<'static> {
    let madt = boot.acpi().unwrap().madt().unwrap();
    let address = madt.local_controller_address().unwrap();
    let physical = address.range(ByteLength::new(4096)).unwrap();
    // The q35 loader's retained identity mappings include this local APIC
    // page with UC attributes. This fixture owns the actual original ICR.
    unsafe { DeviceWindow::admit(physical, InternalAddress::new(address.get())) }.unwrap()
}

pub fn local_controller(
    boot: &boot::Boot<'_>,
    window: DeviceWindow<'static>,
) -> LocalApic<'static> {
    let madt = boot.acpi().unwrap().madt().unwrap();
    let probe = Probe::native();
    let (window, reason) = unsafe { LocalApic::admit(usize::MAX, madt, window, probe) }
        .err()
        .expect("wrong Hart must retain the window");
    assert_eq!(reason, apic::Failure::Scope);
    let controller = unsafe { LocalApic::admit(boot.hart(), madt, window, probe) }
        .ok()
        .expect("actual local APIC");
    assert_eq!(controller.state(), apic::State::Idle);
    assert!(!controller.can_start(boot.hart()));
    assert!(!controller.can_start(255));
    controller
}

pub(super) fn startup(boot: &boot::Boot<'_>) -> boot::Startup {
    boot::Startup::Xapic {
        controller: local_controller(boot, local_window(boot)),
        trampoline: NonNull::new(TRAMPOLINE.load(Ordering::Acquire))
            .unwrap()
            .cast(),
        delay,
    }
}

pub(super) fn acpi_root() -> Option<PhysicalAddress> {
    uefi::system::with_config_table(|entries| {
        let mut roots = entries
            .iter()
            .filter(|entry| entry.guid == uefi::table::cfg::ConfigTableEntry::ACPI2_GUID);
        let root = roots.next()?.address;
        (!root.is_null() && roots.next().is_none())
            .then(|| PhysicalAddress::new(root.addr() as u64))
    })
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
    OLD_IDT.store(Idtr::read().base, Ordering::Relaxed);
    Ok(None)
}

/// Save the TSC calibration while the firmware Stall service is available.
pub fn calibrate() -> Result<(), Status> {
    let start = tsc();
    uefi::boot::stall(Duration::from_millis(10));
    let ticks = tsc()
        .checked_sub(start)
        .filter(|v| *v != 0)
        .ok_or(Status::DEVICE_ERROR)?;
    TICKS_PER_US.store(ticks.div_ceil(10_000), Ordering::Release);
    Ok(())
}

/// Reserve a low, executable startup page before firmware services exit.
pub fn prepare_minor() -> Result<(), Status> {
    calibrate()?;
    let page = uefi::boot::allocate_pages(
        uefi::boot::AllocateType::MaxAddress(0xfffff),
        uefi::boot::MemoryType::LOADER_CODE,
        1,
    )
    .map_err(|e| e.status())?;
    TRAMPOLINE.store(page.as_ptr(), Ordering::Release);
    Ok(())
}

#[repr(C, packed)]
pub struct Idtr {
    pub limit: u16,
    pub base: usize,
}

impl Idtr {
    pub fn read() -> Self {
        let mut value = Self { limit: 0, base: 0 };
        unsafe { asm!("sidt [{}]", in(reg) &mut value, options(nostack, preserves_flags)) };
        value
    }
}

pub(super) fn check_discovery(boot: &boot::Boot<'_>, target: usize) {
    let acpi = boot.acpi().expect("real firmware ACPI");
    let madt = acpi.madt().expect("MADT describes q35 topology");
    assert!(madt.enabled_local_apic(u8::try_from(boot.hart()).unwrap()));
    assert!(madt.enabled_local_apic(u8::try_from(target).unwrap()));
    assert!(madt.io_apics().next().is_some());
    assert!(madt.source_overrides().next().is_some());
    let early = boot.exclusions();
    for physical in core::iter::once(acpi.rsdp_range())
        .chain(core::iter::once(acpi.xsdt().physical()))
        .chain(acpi.tables().map(|table| table.physical()))
    {
        assert!(early.entries().any(|entry| {
            entry.occupant == boot::Occupant::Acpi && entry.physical == physical && entry.fixed()
        }));
    }
    assert!(boot.conventional_ram().is_none());
    boot::write("PASS firmware-discovery q35 acpi=1 madt=1 routes=1 retained=1\n");
}

pub fn inspect(boot: &boot::Boot<'_>, _: &impl MemoryMap) -> boot::Startup {
    assert!(boot.device_tree().is_none());
    assert!(boot.dtb_memory().is_none());
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
    startup(boot)
}

pub fn inspect_started(boot::Startup::Xapic { controller, .. }: &boot::Startup, _: usize) {
    // ICR completion alone would not prove the AP entered. The shared
    // fixture has already observed the actual minor Ready.
    assert_eq!(controller.state(), apic::State::Idle);
    boot::write("PASS local-apic startup=1 scope-rejected=1 target-rejected=1\n");
}
