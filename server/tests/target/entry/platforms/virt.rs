use core::{arch::asm, fmt::Write};
use kernel::entry::boot;
use kernel::metal::{
    firmware::devicetree::{DeviceTree, Error as TreeError},
    firmware::psci::{Conduit, PsciDescription},
    hart::aarch64::description::Aarch64Cpus,
    interrupt::gic::{GicV3, Trigger},
    time::aarch64::GenericTimer,
};
use uefi::{
    Status,
    boot::PAGE_SIZE,
    mem::memory_map::{MemoryMap, MemoryType},
};

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use super::memory;

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
pub(super) fn startup() -> boot::Startup {
    boot::Startup::Psci(conduit())
}
pub(super) static SERVICES: boot::Services = boot::Services {
    output: Some(|b| unsafe { pl011::write_byte(UART.load(Ordering::Acquire), b) }),
    reset: Some(|_| unsafe { conduit().system_off() }),
};

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
        psci.verify().map_err(|_| Status::UNSUPPORTED)?;
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

pub(super) fn hart() -> usize {
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

pub(super) fn inspect(boot: boot::Boot, map: &impl MemoryMap) -> ! {
    let (el, daif, sp, vectors, spsel): (usize, usize, usize, usize, usize);
    unsafe {
        asm!("mrs {}, CurrentEL", "mrs {}, DAIF", "mov {}, sp", "mrs {}, VBAR_EL1",
             "mrs {}, SPSel", out(reg) el, out(reg) daif, out(reg) sp, out(reg) vectors,
             out(reg) spsel, options(nostack, nomem, preserves_flags));
    }
    assert!(el == 4 && daif & 0x3c0 == 0x3c0 && spsel == 1);
    assert!(boot::stack_range().contains(&sp));
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
    inspect_firmware(&firmware, boot.hart());

    let description = boot.dtb_memory().expect("decoded DTB memory");
    let bank = memory::extent(0x4000_0000, 0x5000_0000);
    assert!(description.ram().eq([bank]));
    let tree = memory::extent(tree.start as u64, tree.end as u64);
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
    let mut console = boot::console().expect("board console");
    console
        .write_str("PASS firmware-discovery virt dtb=1 psci=1 affinity=1 gic=1 timer=1\n")
        .expect("firmware discovery output");
    console
        .write_str("PASS virt major=1 minor=1 ebs=1 el1=1 masked=1 stack=1 vectors=1 dtb=1 ram=1 reconciled=1\n")
        .expect("diagnostic output");
    boot::shutdown(true)
}

fn inspect_firmware(tree: &DeviceTree<'_>, major: usize) {
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
    unsafe {
        psci.verify().expect("live PSCI version and calls");
        psci.verify_affinity(target, true)
            .expect("live minor affinity");
    }

    let timer = GenericTimer::read(tree)
        .unwrap()
        .expect("generic timer declaration");
    assert_eq!(tree.node(timer.source).unwrap().name(), b"timer");
    assert_eq!(timer.virtual_intid, 27);
    assert_eq!(timer.virtual_trigger, Trigger::LevelHigh);
    assert!(timer.always_on);
    assert!(tree.node(timer.controller.source).is_some());
    assert_eq!(
        timer.controller.distributor,
        memory::extent(0x0800_0000, 0x0801_0000)
    );
    assert!(
        timer
            .controller
            .redistributor
            .covers(memory::extent(0x080a_0000, 0x080c_0000))
    );
    assert!(unsafe { GicV3::cpu_interface_available() });
    let _frequency = unsafe { timer.counter_frequency() }.expect("live generic timer frequency");
}
