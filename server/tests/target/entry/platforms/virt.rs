use core::{arch::asm, fmt::Write};
use kernel::entry::boot;
use uefi::{
    Status,
    boot::{self as firmware, AllocateType, PAGE_SIZE},
    guid,
    mem::memory_map::{MemoryMap, MemoryType},
};

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use super::memory;

#[path = "pl011.rs"]
mod pl011;
static FIRMWARE_VECTORS: AtomicUsize = AtomicUsize::new(0);
static UART: AtomicUsize = AtomicUsize::new(0);
static USE_HVC: AtomicBool = AtomicBool::new(false);

fn conduit() -> boot::Conduit {
    if USE_HVC.load(Ordering::Acquire) {
        boot::Conduit::Hvc
    } else {
        boot::Conduit::Smc
    }
}
pub(super) fn startup() -> boot::Startup {
    boot::Startup::Psci(conduit())
}
pub(super) static SERVICES: boot::Services = boot::Services {
    output: Some(|b| unsafe { pl011::write_byte(UART.load(Ordering::Acquire), b) }),
    reset: Some(|_| unsafe { conduit().system_off() }),
};

fn discover(bytes: &[u8]) -> Result<(), Status> {
    let tree = fdt::Fdt::new(bytes).map_err(|_| Status::COMPROMISED_DATA)?;
    let psci = tree
        .find_compatible(&["arm,psci-1.0", "arm,psci-0.2"])
        .ok_or(Status::NOT_FOUND)?;
    let hvc = match psci.property("method").and_then(|p| p.as_str()) {
        Some("hvc") => true,
        Some("smc") => false,
        _ => return Err(Status::UNSUPPORTED),
    };
    tree.find_node("/chosen").ok_or(Status::NOT_FOUND)?;
    tree.find_node("/cpus").ok_or(Status::NOT_FOUND)?;
    let serial = tree
        .chosen()
        .stdout()
        .filter(|node| {
            node.compatible()
                .is_some_and(|c| c.all().any(|s| s == "arm,pl011"))
        })
        .ok_or(Status::NOT_FOUND)?;
    let range = serial
        .reg()
        .and_then(|mut r| r.next())
        .filter(|r| {
            !r.starting_address.is_null()
                && r.starting_address.addr().is_multiple_of(4)
                && r.size.is_some_and(|s| s >= 0x1c)
        })
        .ok_or(Status::COMPROMISED_DATA)?;
    let target = tree
        .cpus()
        .filter(|cpu| {
            cpu.property("reg").is_some()
                && cpu.property("enable-method").and_then(|p| p.as_str()) == Some("psci")
                && cpu
                    .property("status")
                    .is_none_or(|p| matches!(p.as_str(), Some("okay" | "ok")))
        })
        .flat_map(|cpu| cpu.ids().all())
        .find(|id| *id != hart())
        .ok_or(Status::NOT_FOUND)?;
    USE_HVC.store(hvc, Ordering::Release);
    UART.store(range.starting_address.addr(), Ordering::Release);
    crate::TARGET.store(target, Ordering::Release);
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
    let source = uefi::system::with_config_table(|entries| {
        entries
            .iter()
            .find(|entry| entry.guid == guid!("b1b621d5-f19c-41a5-830b-d9152c69aae0"))
            .map(|entry| entry.address.cast::<u8>())
    })
    .ok_or(Status::NOT_FOUND)?;
    let map = firmware::memory_map(MemoryType::LOADER_DATA).map_err(|e| e.status())?;
    // AAVMF can publish the DTB in Boot Services data. Copy it while the
    // firmware still owns the source and reserve the copy across the handoff.
    let size = unsafe {
        dtb_size(
            source,
            &map,
            &[MemoryType::BOOT_SERVICES_DATA, MemoryType::ACPI_RECLAIM],
        )
    }?;
    discover(unsafe { core::slice::from_raw_parts(source, size) })?;
    let dtb = firmware::allocate_pages(
        AllocateType::AnyPages,
        MemoryType::LOADER_DATA,
        size.div_ceil(PAGE_SIZE),
    )
    .map_err(|e| e.status())?;
    unsafe { source.copy_to_nonoverlapping(dtb.as_ptr(), size) };
    Ok(Some(unsafe {
        core::slice::from_raw_parts(dtb.as_ptr(), size)
    }))
}

// The caller supplies a firmware table pointer or its reserved, identity-mapped copy.
unsafe fn dtb_size(
    dtb: *const u8,
    map: &impl MemoryMap,
    types: &[MemoryType],
) -> Result<usize, Status> {
    let addr = dtb.addr();
    if dtb.is_null() || !addr.is_multiple_of(8) {
        return Err(Status::COMPROMISED_DATA);
    }
    let available = map
        .entries()
        .filter(|d| types.contains(&d.ty))
        .find_map(|desc| {
            let end = desc
                .phys_start
                .checked_add(desc.page_count.checked_mul(PAGE_SIZE as u64)?)?;
            (desc.phys_start..end)
                .contains(&(addr as u64))
                .then(|| end as usize - addr)
        })
        .ok_or(Status::COMPROMISED_DATA)?;
    if available < 40 {
        return Err(Status::COMPROMISED_DATA);
    }
    let words = dtb.cast::<u32>();
    let magic = u32::from_be(unsafe { words.read() });
    let size = u32::from_be(unsafe { words.add(1).read() }) as usize;
    if magic != 0xd00dfeed || !(40..=available).contains(&size) {
        return Err(Status::COMPROMISED_DATA);
    }
    Ok(size)
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
    let size = unsafe { dtb_size(tree.start as *const u8, map, &[MemoryType::LOADER_DATA]) };
    assert_eq!(size, Ok(tree.len()));

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
    boot::console()
        .expect("board console")
        .write_str("PASS virt major=1 minor=1 ebs=1 el1=1 masked=1 stack=1 vectors=1 dtb=1 ram=1 reconciled=1\n")
        .expect("diagnostic output");
    boot::shutdown(true)
}
