use core::{arch::asm, fmt::Write};
use kernel::entry::boot;
use kernel::metal::{
    chrono::aarch64::GenericCounter,
    interrupt::internal::{
        architected::GenericTimer,
        gic::{GicV3, PrivateInterrupts, Trigger},
    },
    platform::description::devicetree::{DeviceTree, Error as TreeError},
    platform::firmware::psci::{Conduit, Psci, PsciDescription},
    processor::hart::aarch64::description::Aarch64Cpus,
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
#[path = "gic.rs"]
mod gic;
#[path = "pl011.rs"]
mod pl011;
#[path = "stream.rs"]
mod stream;
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

pub(super) fn inspect(boot: &boot::Boot<'_>, map: &impl MemoryMap) -> boot::Startup {
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
    inspect_smmu(&firmware);
    let psci = inspect_firmware(&firmware, boot.hart());

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
    boot::Startup::Psci(psci)
}

pub(super) fn report() -> ! {
    let mut console = boot::console().expect("board console");
    console
        .write_str("PASS virt major=1 minor=1 ebs=1 el1=1 masked=1 stack=1 vectors=1 dtb=1 ram=1 reconciled=1\n")
        .expect("diagnostic output");
    boot::shutdown(true)
}

fn inspect_smmu(tree: &DeviceTree<'_>) {
    use kernel::metal::memory::mmu::{external::smmu, internal::device::DeviceWindow};
    use limen_treaty::model::InternalAddress;
    let mut range = None;
    for node in tree.nodes() {
        if let Some(registers) = smmu::registers(node).unwrap() {
            assert!(range.replace(registers).is_none());
        }
    }
    let range = range.expect("virt SMMUv3");
    // AAVMF retains this device mapping after ExitBootServices. The guest
    // owns the test endpoint and keeps the firmware disk quiescent after exit.
    let mut registers =
        unsafe { DeviceWindow::admit(range, InternalAddress::new(range.start().get())).unwrap() };
    let profile = unsafe { smmu::probe(&mut registers) }.unwrap();
    assert!(profile.stage_two && profile.granule_4k && !profile.enabled);
    fn order() {
        unsafe { asm!("dsb sy", options(nostack, preserves_flags)) };
    }
    let mut endpoint = stream::Endpoint::open(tree, range);
    endpoint.prime();
    let mut change = unsafe { smmu::abort_bypass(&mut registers, order) }.unwrap();
    fn retry<T>(mut f: impl FnMut() -> Result<T, smmu::Error>) -> T {
        for _ in 0..100_000 {
            match f() {
                Ok(value) => return value,
                Err(smmu::Error::Busy) => core::hint::spin_loop(),
                Err(e) => panic!("SMMUv3 control failed: {e:?}"),
            }
        }
        panic!("SMMUv3 acknowledgement timeout");
    }
    retry(|| change.observe());
    endpoint.verify_abort();
    endpoint.install(&mut registers, order);
    use limen_treaty::model::PhysicalAddress;
    use smmu::queue::{Invalidate, Queue, Ring};
    static mut COMMANDS: Ring = Ring::EMPTY;
    let commands = unsafe { &mut *(&raw mut COMMANDS) };
    let physical = PhysicalAddress::new(core::ptr::from_ref(commands).addr() as u64);
    let mut queue = unsafe { Queue::open(&mut registers, commands, physical, order) }.unwrap();
    for operation in [
        Invalidate::Stream(0),
        Invalidate::StageTwo(1),
        Invalidate::Stream(1),
        Invalidate::StageTwo(2),
    ] {
        retry(|| queue.submit(operation));
        assert_eq!(queue.submit(operation), Err(smmu::Error::Active));
        assert_eq!(retry(|| queue.observe()), operation);
        assert_eq!(queue.observe(), Err(smmu::Error::Active));
    }
    endpoint.verify_translation();
    endpoint.downgrade();
    let invalidate = Invalidate::StageTwo(1);
    retry(|| queue.submit(invalidate));
    assert_eq!(retry(|| queue.observe()), invalidate);
    endpoint.verify_downgrade();
    retry(|| queue.shut());
    boot::write("\nPASS smmuv3 stage2=1 granule=4096 abort=1 sync=5 dma=1 downgrade=1\n");
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
    assert_eq!(
        controller.distributor(),
        memory::extent(0x0800_0000, 0x0801_0000)
    );
    assert!(
        controller
            .redistributor()
            .covers(memory::extent(0x080a_0000, 0x080c_0000))
    );
    assert!(unsafe { GicV3::cpu_interface_available() });
    gic::check(tree, controller, major);
    let _frequency = unsafe { counter.counter_frequency() }.expect("live generic timer frequency");
    psci
}

/// Recheck the original PSCI target only after the minor has entered.
pub(super) fn inspect_started(boot::Startup::Psci(psci): &boot::Startup, target: usize) {
    unsafe { psci.verify_affinity(target, true) }.expect("live minor affinity");
}
