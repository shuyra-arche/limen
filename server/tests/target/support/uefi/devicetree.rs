use uefi::{
    Status,
    boot::{self as firmware, AllocateType, PAGE_SIZE},
    guid,
    mem::memory_map::{MemoryMap, MemoryType},
};

const MAX_DTB_BYTES: usize = 2 * 1024 * 1024;

pub(super) fn copy(
    verify: impl FnOnce(&[u8]) -> Result<(), Status>,
) -> Result<&'static [u8], Status> {
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
    let pages = size.div_ceil(PAGE_SIZE);
    let dtb = firmware::allocate_pages(AllocateType::AnyPages, MemoryType::LOADER_DATA, pages)
        .map_err(|e| e.status())?;
    unsafe { source.copy_to_nonoverlapping(dtb.as_ptr(), size) };
    let bytes = unsafe { core::slice::from_raw_parts(dtb.as_ptr(), size) };
    if let Err(status) = verify(bytes) {
        unsafe { firmware::free_pages(dtb, pages) }.map_err(|error| error.status())?;
        return Err(status);
    }
    Ok(bytes)
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
    if magic != 0xd00dfeed || !(40..=available.min(MAX_DTB_BYTES)).contains(&size) {
        return Err(Status::COMPROMISED_DATA);
    }
    Ok(size)
}
