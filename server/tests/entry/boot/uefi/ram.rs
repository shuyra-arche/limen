use core::{
    arch::asm,
    sync::atomic::{AtomicUsize, Ordering},
};

use kernel::{
    entry::boot::{Boot, Condition, Retirement},
    metal::memory::allocator::treaty::RegionOrigin,
};
use limen_treaty::model::ByteLength;
use uefi::{
    Status,
    boot::{self as firmware, AllocateType, MemoryType, PAGE_SIZE},
    mem::memory_map::MemoryMap,
};

use super::{foundation, memory};

#[cfg(target_arch = "aarch64")]
#[path = "../../../metal/memory/mmu/aarch64/owned.rs"]
mod upper;

#[cfg(target_arch = "aarch64")]
const UPPER_PAGES: usize = upper::PAGES;
#[cfg(target_arch = "x86_64")]
const UPPER_PAGES: usize = 0;
const PAGES: usize = foundation::PAGES + UPPER_PAGES;
const BYTES: usize = PAGES * PAGE_SIZE;
static WINDOW: AtomicUsize = AtomicUsize::new(0);
static ROOT: AtomicUsize = AtomicUsize::new(0);

fn translation_root() -> usize {
    let root;
    #[cfg(target_arch = "x86_64")]
    unsafe {
        asm!("mov {}, cr3", out(reg) root, options(nomem, nostack, preserves_flags));
    }
    #[cfg(target_arch = "aarch64")]
    unsafe {
        asm!("mrs {}, ttbr0_el1", out(reg) root, options(nomem, nostack, preserves_flags));
    }
    root
}

pub(super) fn prepare() -> Result<(), Status> {
    let map = firmware::memory_map(MemoryType::LOADER_DATA).map_err(|error| error.status())?;
    // Select from the real map. Use the low end of the largest conventional
    // descriptor so allocating the final map at the high end leaves room.
    let start = map
        .entries()
        .filter(|entry| entry.ty == MemoryType::CONVENTIONAL && entry.phys_start >= 0x10_0000)
        .max_by_key(|entry| entry.page_count)
        .filter(|entry| entry.page_count >= PAGES as u64)
        .ok_or(Status::NOT_FOUND)?
        .phys_start;
    drop(map);
    let pages =
        firmware::allocate_pages(AllocateType::Address(start), MemoryType::LOADER_DATA, PAGES)
            .map_err(|error| error.status())?;
    assert_eq!(pages.as_ptr().addr() as u64, start);
    // This is the fixture's unused private grant. The firmware may scrub it
    // on free. The final map must establish that it stayed conventional RAM.
    unsafe { firmware::free_pages(pages, PAGES) }.map_err(|error| error.status())?;
    ROOT.store(translation_root(), Ordering::Relaxed);
    WINDOW.store(pages.as_ptr().addr(), Ordering::Release);
    Ok(())
}

pub(super) fn check(boot: &Boot<'_>) {
    let start = WINDOW.load(Ordering::Acquire);
    assert_ne!(start, 0);
    assert_eq!(translation_root(), ROOT.load(Ordering::Relaxed));
    let window = memory::extent(start as u64, (start + BYTES) as u64);
    let map = boot.firmware_map().unwrap();
    assert!(map.regions().any(|region| {
        region.kind == MemoryType::CONVENTIONAL.0 && region.physical.covers(window)
    }));
    assert!(
        boot.exclusions()
            .entries()
            .all(|entry| !entry.physical.overlaps(window))
    );

    let foundation_end = start + foundation::PAGES * PAGE_SIZE;
    let scope = memory::extent(start as u64, foundation_end as u64);
    let mut evidence = Retirement::new(scope);
    let mut descriptions = [None; 1];
    // The scope was granted to this guest as conventional RAM and stayed
    // conventional through the final map. It contained no firmware stack,
    // translation tables, or device buffers. The QEMU fixture never submits
    // the grant to a device or to another processor. The unchanged root
    // still provides the canonical view, and no old translation user of
    // this particular extent survives the firmware handoff.
    unsafe {
        evidence.record(Condition::NoEarlyBorrow);
        evidence.record(Condition::TranslationRetired);
    }
    let granule = ByteLength::new(PAGE_SIZE as u64);
    assert!(
        boot.regions(granule, Some(&evidence), &mut descriptions)
            .unwrap()
            .entries()
            .next()
            .is_none()
    );
    unsafe { evidence.record(Condition::DevicesIsolated) };
    let qualified = boot
        .regions(granule, Some(&evidence), &mut descriptions)
        .unwrap();
    let region = qualified.entries().next().unwrap();
    assert_eq!(region.physical, scope);
    #[cfg(target_arch = "x86_64")]
    assert!(matches!(region.origin, RegionOrigin::Uefi { .. }));
    #[cfg(target_arch = "aarch64")]
    assert!(matches!(
        region.origin,
        RegionOrigin::DeviceTreeAndUefi { .. }
    ));

    unsafe { foundation::check(boot, scope.start().get(), &evidence) };

    #[cfg(target_arch = "aarch64")]
    {
        let range = memory::extent(foundation_end as u64, window.end().get());
        let mut evidence = Retirement::new(range);
        // This is the remaining independent extent of the same unexposed
        // firmware window. The original allocator never admitted these pages.
        unsafe {
            evidence.record(Condition::NoEarlyBorrow);
            evidence.record(Condition::TranslationRetired);
            evidence.record(Condition::DevicesIsolated);
            upper::check(boot, range, &evidence);
        }
    }
}
