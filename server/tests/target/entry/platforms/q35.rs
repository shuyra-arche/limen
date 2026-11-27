use core::arch::asm;
use core::sync::atomic::{AtomicUsize, Ordering};
use kernel::entry::boot;
use kernel::metal::{
    interrupt::internal::apic::{self, LocalApic, Probe},
    memory::mmu::internal::device::DeviceWindow,
};
use limen_treaty::model::{ByteLength, InternalAddress, PhysicalAddress};
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
#[path = "serial_irq.rs"]
mod serial_irq;

#[path = "amdvi.rs"]
mod amdvi;

#[path = "edu.rs"]
mod edu;

#[path = "vtd.rs"]
mod vtd;

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

fn startup(boot: &boot::Boot<'_>) -> boot::Startup {
    let madt = boot.acpi().unwrap().madt().unwrap();
    let address = madt.local_controller_address().unwrap();
    let physical = address.range(ByteLength::new(4096)).unwrap();
    // The q35 loader's retained identity mappings include this local APIC
    // page with UC attributes. This fixture owns the actual original ICR.
    let window =
        unsafe { DeviceWindow::admit(physical, InternalAddress::new(address.get())) }.unwrap();
    let mut window = window;
    unsafe { serial_irq::prepare_local(&mut window) };
    let probe = Probe::native();
    let (window, reason) = unsafe { LocalApic::admit(usize::MAX, madt, window, probe) }
        .err()
        .expect("wrong Hart must retain the window");
    assert_eq!(reason, apic::Failure::Scope);
    let mut controller = unsafe { LocalApic::admit(boot.hart(), madt, window, probe) }
        .ok()
        .expect("actual local APIC");
    assert_eq!(controller.state(), apic::State::Idle);
    assert!(!controller.can_start(boot.hart()));
    assert!(!controller.can_start(255));
    serial_irq::check(madt, boot.hart(), &mut controller);
    boot::Startup::Xapic {
        controller,
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

pub(super) fn check_discovery(boot: &boot::Boot<'_>, target: usize) {
    let acpi = boot.acpi().expect("real firmware ACPI");
    let madt = acpi.madt().expect("MADT describes q35 topology");
    assert!(madt.enabled_local_apic(u8::try_from(boot.hart()).unwrap()));
    assert!(madt.enabled_local_apic(u8::try_from(target).unwrap()));
    assert!(madt.io_apics().next().is_some());
    assert!(madt.source_overrides().next().is_some());
    if cfg!(feature = "apic-failure") {
        amdvi::check(
            acpi.ivhd()
                .expect("valid IVRS")
                .expect("AMD remapping hardware"),
        );
    } else {
        let dmar = acpi.dmar().unwrap().expect("q35 intel-iommu DMAR");
        let mut units = dmar.units();
        let unit = units.next().expect("q35 DRHD");
        assert!(
            units.next().is_none()
                && unit.segment() == 0
                && (unit.includes_all_pci() || unit.lists_root_endpoint(0, 6, 0))
        );
        // This board fixture installs the chipset VT-d aperture at this exact
        // identity-mapped UC page. Check firmware against the board before MMIO.
        let address = PhysicalAddress::new(0xfed9_0000);
        assert_eq!(
            unit.registers(),
            address.range(ByteLength::new(4096)).unwrap()
        );
        let mut registers =
            unsafe { DeviceWindow::admit(unit.registers(), InternalAddress::new(address.get())) }
                .unwrap();
        vtd::check(unit, &mut registers);
    }
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
}

pub(super) fn inspect(boot: &boot::Boot<'_>, _: &impl MemoryMap) -> boot::Startup {
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
    boot::write("PASS firmware-discovery q35 acpi=1 madt=1 routes=1 retained=1\n");
    startup(boot)
}

pub(super) fn report() -> ! {
    use core::fmt::Write;
    boot::console()
        .expect("board console")
        .write_str("PASS q35 major=1 minor=1 ebs=1 cpl0=1 paging=1 masked=1 stack=1 idt=1\n")
        .expect("diagnostic output");
    boot::shutdown(true)
}

pub(super) fn inspect_started(boot::Startup::Xapic { controller, .. }: &boot::Startup, _: usize) {
    // ICR completion alone would not prove the AP entered. The shared
    // cooperative fixture has already observed the actual minor Ready.
    assert_eq!(controller.state(), apic::State::Idle);
    boot::write("PASS local-apic startup=1 scope-rejected=1 target-rejected=1\n");
}

static FAILURE_DELAY_CALLS: AtomicUsize = AtomicUsize::new(0);
static OTHER_MINOR: boot::Minor = boot::Minor::new();

fn reject_before_send(_: u32) -> bool {
    false
}

fn reject_after_init(microseconds: u32) -> bool {
    match FAILURE_DELAY_CALLS.fetch_add(1, Ordering::Relaxed) {
        0 => {
            assert_eq!(microseconds, 1);
            delay(microseconds)
        }
        1 => {
            assert_eq!(microseconds, 10_000);
            false
        }
        _ => panic!("a later command followed the uncertain INIT"),
    }
}

fn forbidden(_: usize, _: usize) -> ! {
    panic!("failure fixture must never enter the target")
}

pub(super) fn check_failure(startup: &mut boot::Startup, target: usize) {
    let boot::Startup::Xapic {
        trampoline, delay, ..
    } = startup;
    let page = PhysicalAddress::new(trampoline.addr().get() as u64);
    *delay = reject_before_send;
    // MP Services identified this inactive target. No SIPI will be sent
    // in either attempt, and the original reserved low page remains live.
    assert_eq!(
        unsafe { super::MINOR.start(target, forbidden, 0, startup) },
        Err(boot::StartError::Rejected(-1))
    );
    let boot::Startup::Xapic {
        controller, delay, ..
    } = startup;
    assert_eq!(controller.state(), apic::State::Idle);
    assert!(!super::MINOR.ready());
    *delay = reject_after_init;

    assert_eq!(
        unsafe { super::MINOR.start(target, forbidden, 0, startup) },
        Err(boot::StartError::Unconfirmed(-1))
    );
    assert_eq!(FAILURE_DELAY_CALLS.load(Ordering::Relaxed), 2);
    let expected = apic::State::Unconfirmed(apic::Effect {
        hart: u8::try_from(target).unwrap(),
        page,
        stage: apic::Stage::Init,
    });
    let boot::Startup::Xapic { controller, .. } = startup;
    assert_eq!(controller.state(), expected);
    assert!(!super::MINOR.ready());

    // The original launch is retained. A second unused launch record also
    // cannot use the uncertain ICR or rewrite the original trampoline.
    assert_eq!(
        unsafe { super::MINOR.start(target, forbidden, 1, startup) },
        Err(boot::StartError::Busy)
    );
    assert_eq!(
        unsafe { OTHER_MINOR.start(target, forbidden, 2, startup) },
        Err(boot::StartError::Rejected(-1))
    );
    let boot::Startup::Xapic { controller, .. } = startup;
    assert_eq!(controller.state(), expected);
    assert_eq!(FAILURE_DELAY_CALLS.load(Ordering::Relaxed), 2);
    assert!(!OTHER_MINOR.ready());
    boot::write("PASS local-apic-failure before=1 after-init=1 retained=1 blocked=2\n");
}
