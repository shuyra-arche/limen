use core::{arch::asm, fmt::Write};
use kernel::entry::boot;
use uefi::{
    Status,
    boot::{self as firmware, AllocateType, PAGE_SIZE},
    guid,
    mem::memory_map::{MemoryMap, MemoryType},
};

pub(super) fn hart() -> usize {
    let mpidr: usize;
    unsafe { asm!("mrs {}, MPIDR_EL1", out(reg) mpidr, options(nomem, nostack)) };
    mpidr & 0xff00ffffff
}

pub(super) fn capture() -> Result<Option<&'static [u8]>, Status> {
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
    unsafe extern "C" {
        static limen_vectors: u8;
    }
    assert!(vectors == (&raw const limen_vectors) as usize && vectors & 2047 == 0);
    let tree = boot.device_tree().expect("UEFI virt DTB");
    let size = unsafe { dtb_size(tree.start as *const u8, map, &[MemoryType::LOADER_DATA]) };
    assert_eq!(size, Ok(tree.len()));
    boot::console()
        .expect("board console")
        .write_str("PASS virt major=1 minor=1 ebs=1 el1=1 masked=1 stack=1 vectors=1 dtb=1\n")
        .expect("diagnostic output");
    boot::shutdown(true)
}
