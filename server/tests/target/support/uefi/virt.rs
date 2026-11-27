use core::arch::asm;
use core::fmt::Write;
use kernel::entry::boot;
use kernel::metal::interrupt::aarch64::gic::GicV3;
use kernel::metal::platform::boot::treaty as boot_diagnostic;
use kernel::metal::processor::treaty::Startup;
use kernel::metal::{
    chrono::aarch64::description::GenericCounter,
    interrupt::aarch64::{
        gic::{Trigger, cpu_interface, redistributor::input::description::PrivateInterrupts},
        timer::GenericTimer,
    },
    platform::firmware::treaty::psci::Psci,
};
use kernel::metal::{
    platform::description::treaty::devicetree::{DeviceTree, Error as TreeError},
    platform::firmware::treaty::psci::{Conduit, PsciDescription},
    processor::description::aarch64::cpu::Aarch64Cpus,
};
use uefi::Status;
use uefi::{
    boot::PAGE_SIZE,
    mem::memory_map::{MemoryMap, MemoryType},
};

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use super::extent;

#[path = "devicetree.rs"]
mod devicetree;
#[path = "pl011.rs"]
mod pl011;
static FIRMWARE_VECTORS: AtomicUsize = AtomicUsize::new(0);
static UART: AtomicUsize = AtomicUsize::new(0);
static USE_HVC: AtomicBool = AtomicBool::new(false);

fn conduit() -> Conduit {
    if USE_HVC.load(Ordering::Acquire) {
        Conduit::Hvc
    } else {
        Conduit::Smc
    }
}
pub(super) static TERMINAL: kernel::metal::processor::treaty::Terminal =
    kernel::metal::processor::treaty::Terminal::new(
        Some(|b| unsafe { pl011::write_byte(UART.load(Ordering::Acquire), b) }),
        Some(|_| unsafe { conduit().system_off() }),
    );

fn checked<T>(result: Result<T, TreeError>) -> Result<T, Status> {
    result.map_err(|error| match error {
        TreeError::Unsupported => Status::UNSUPPORTED,
        TreeError::Missing => Status::NOT_FOUND,
        _ => Status::COMPROMISED_DATA,
    })
}

fn discover(bytes: &[u8]) -> Result<(), Status> {
    let tree = checked(DeviceTree::new(bytes))?;
    let psci = checked(PsciDescription::read(&tree))?.ok_or(Status::NOT_FOUND)?;
    let cpus = checked(Aarch64Cpus::read(&tree))?.ok_or(Status::NOT_FOUND)?;
    let major = checked(unsafe { cpus.current() })?;
    let target = cpus
        .candidates()
        .find(|cpu| cpu.enabled && cpu.starts_with_psci && cpu.affinity != major.affinity)
        .ok_or(Status::NOT_FOUND)?;
    // AAVMF's retained native tree describes the conduit of this running VM.
    // Probe the calls before installing PSCI as a startup and shutdown service.
    unsafe {
        let psci = psci.verify().map_err(|_| Status::UNSUPPORTED)?;
        psci.verify_affinity(major.affinity, true)
            .map_err(|_| Status::COMPROMISED_DATA)?;
        psci.verify_affinity(target.affinity, false)
            .map_err(|_| Status::COMPROMISED_DATA)?;
    }

    let stdout = checked(tree.find_path(b"/chosen"))?.ok_or(Status::NOT_FOUND)?;
    let stdout = checked(stdout.property(b"stdout-path"))?
        .and_then(|bytes| bytes.strip_suffix(&[0]))
        .and_then(|value| value.split(|byte| *byte == b':').next())
        .ok_or(Status::NOT_FOUND)?;
    let path = if stdout.starts_with(b"/") {
        stdout
    } else {
        let aliases = checked(tree.find_path(b"/aliases"))?.ok_or(Status::NOT_FOUND)?;
        checked(aliases.property(stdout))?
            .and_then(|bytes| bytes.strip_suffix(&[0]))
            .ok_or(Status::NOT_FOUND)?
    };
    let serial = checked(tree.find_path(path))?.ok_or(Status::NOT_FOUND)?;
    if !checked(serial.enabled())? || !checked(serial.compatible(b"arm,pl011"))? {
        return Err(Status::UNSUPPORTED);
    }
    let range = checked(serial.register(0))?;
    let start = range.start().get();
    if start == 0 || !start.is_multiple_of(4) || range.end().get() - start < 0x1c {
        return Err(Status::COMPROMISED_DATA);
    }
    USE_HVC.store(psci.conduit() == Conduit::Hvc, Ordering::Release);
    UART.store(
        usize::try_from(start).map_err(|_| Status::COMPROMISED_DATA)?,
        Ordering::Release,
    );
    crate::TARGET.store(target.affinity, Ordering::Release);
    Ok(())
}

pub fn hart() -> usize {
    let mpidr: usize;
    unsafe { asm!("mrs {}, MPIDR_EL1", out(reg) mpidr, options(nomem, nostack)) };
    mpidr & 0xff00ffffff
}

pub(super) fn capture() -> Result<Option<&'static [u8]>, Status> {
    let vectors: usize;
    unsafe { asm!("mrs {}, VBAR_EL1", out(reg) vectors, options(nostack, nomem, preserves_flags)) };
    FIRMWARE_VECTORS.store(vectors, Ordering::Relaxed);
    devicetree::copy(discover).map(Some)
}

pub fn inspect(boot: &boot::Boot<'_>, map: &impl MemoryMap) -> Startup {
    let (el, daif, sp, vectors, spsel): (usize, usize, usize, usize, usize);
    unsafe {
        asm!("mrs {}, CurrentEL", "mrs {}, DAIF", "mov {}, sp", "mrs {}, VBAR_EL1",
             "mrs {}, SPSel", out(reg) el, out(reg) daif, out(reg) sp, out(reg) vectors,
             out(reg) spsel, options(nostack, nomem, preserves_flags));
    }
    assert!(el == 4 && daif & 0x3c0 == 0x3c0 && spsel == 1);
    assert!(boot.stack_range().contains(&sp));
    assert_ne!(vectors, FIRMWARE_VECTORS.load(Ordering::Relaxed));
    assert!(vectors != 0 && vectors.is_multiple_of(2048));
    // All 16 architectural vector slots must fit inside loaded image code.
    let end = vectors.checked_add(2048).unwrap() as u64;
    assert!(map.entries().any(|d| {
        d.ty == MemoryType::LOADER_CODE
            && d.phys_start <= vectors as u64
            && d.page_count
                .checked_mul(PAGE_SIZE as u64)
                .and_then(|bytes| d.phys_start.checked_add(bytes))
                .is_some_and(|limit| end <= limit)
    }));
    let tree = boot.device_tree().expect("UEFI virt DTB");
    let firmware = boot
        .device_tree_description()
        .expect("validated UEFI virt DTB");
    assert_eq!(firmware.bytes().as_ptr().addr(), tree.start);
    assert_eq!(firmware.bytes().len(), tree.len());
    let psci = inspect_firmware(&firmware, boot.hart());

    let description = boot.dtb_memory().expect("decoded DTB memory");
    let bank = extent(0x4000_0000, 0x5000_0000);
    assert!(description.ram().eq([bank]));
    let tree = extent(tree.start as u64, tree.end as u64);
    let mut compared = boot
        .conventional_ram()
        .expect("DTB and final UEFI map")
        .peekable();
    assert!(compared.peek().is_some());
    for region in compared {
        let range = region.physical;
        assert!(bank.covers(range));
        assert!(!range.overlaps(tree));
        assert!(
            description
                .reservations()
                .all(|reserved| !range.overlaps(reserved.physical))
        );
        assert!(map.entries().any(|d| {
            d.ty == MemoryType::CONVENTIONAL
                && d.att.bits() == region.attributes
                && d.phys_start <= range.start().get()
                && range.end().get() <= d.phys_start + d.page_count * PAGE_SIZE as u64
        }));
    }
    let mut console = boot_diagnostic::console().expect("board console");
    console
        .write_str("PASS firmware-discovery virt dtb=1 psci=1 affinity=1 gic=1 timer=1\n")
        .expect("firmware discovery output");
    Startup::Psci(psci)
}

fn inspect_firmware(tree: &DeviceTree<'_>, major: usize) -> Psci {
    let cpus = Aarch64Cpus::read(tree).unwrap().expect("CPU declarations");
    let current = unsafe { cpus.current() }.expect("MPIDR matches an enabled CPU node");
    assert_eq!(current.affinity, major);
    let target = crate::TARGET.load(Ordering::Acquire);
    let minor = cpus
        .candidates()
        .find(|cpu| cpu.affinity == target && cpu.enabled && cpu.starts_with_psci)
        .expect("minor's original PSCI CPU declaration");
    assert_ne!(current.source, minor.source);
    assert!(tree.node(minor.source).is_some());

    let psci = PsciDescription::read(tree)
        .unwrap()
        .expect("PSCI declaration");
    assert!(tree.node(psci.source()).is_some());
    assert_eq!(psci.conduit(), conduit());
    let psci = unsafe { psci.verify() }.expect("live PSCI version and calls");
    unsafe { psci.verify_affinity(target, false) }.expect("minor affinity before startup");

    let counter = GenericCounter::read(tree)
        .unwrap()
        .expect("generic counter declaration");
    assert_eq!(tree.node(counter.source()).unwrap().name(), b"timer");
    let controller = GicV3::read(tree).unwrap().expect("GIC description");
    let interrupts = PrivateInterrupts::read(tree, counter.source(), controller)
        .expect("GIC private interrupts");
    let timer = GenericTimer::describe(tree, &interrupts).expect("virtual timer role");
    let controller = interrupts.controller();
    assert_eq!(timer.source(), counter.source());
    assert_eq!(timer.domain(), controller.source());
    let interrupt = timer.virtual_interrupt();
    assert_eq!(interrupt.intid, 27);
    assert_eq!(interrupt.trigger, Trigger::LevelHigh);
    assert!(timer.always_on());
    assert!(tree.node(controller.source()).is_some());
    assert_eq!(controller.distributor(), extent(0x0800_0000, 0x0801_0000));
    assert!(
        controller
            .redistributor()
            .covers(extent(0x080a_0000, 0x080c_0000))
    );
    assert!(unsafe { cpu_interface::available() });
    let _frequency = unsafe { counter.counter_frequency() }.expect("live generic timer frequency");
    psci
}

/// Recheck the original PSCI target only after the minor has entered.
pub fn inspect_started(Startup::Psci(psci): &Startup, target: usize) {
    unsafe { psci.verify_affinity(target, true) }.expect("live minor affinity");
}
