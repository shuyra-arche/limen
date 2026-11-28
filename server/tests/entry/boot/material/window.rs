//! A private conventional RAM extent for a trusted UEFI guest.

use core::{
    arch::asm,
    sync::atomic::{AtomicUsize, Ordering},
};

use kernel::entry::boot::Boot;
use limen_treaty::model::PhysicalRange;
use uefi::{
    Status,
    boot::{self as firmware, AllocateType, MemoryType, PAGE_SIZE},
    mem::memory_map::MemoryMap,
};

use super::memory::extent;

pub(super) struct Window<const PAGES: usize> {
    start: AtomicUsize,
    root: AtomicUsize,
}

impl<const PAGES: usize> Window<PAGES> {
    pub(super) const fn new() -> Self {
        Self {
            start: AtomicUsize::new(0),
            root: AtomicUsize::new(0),
        }
    }

    pub(super) fn prepare(&self) -> Result<(), Status> {
        assert!(PAGES != 0);
        assert_eq!(self.start.load(Ordering::Acquire), 0);
        let map = firmware::memory_map(MemoryType::LOADER_DATA).map_err(|error| error.status())?;
        // Take the low end so the final map can be allocated at the high end.
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
        // The final map must establish that the unused extent stayed conventional.
        unsafe { firmware::free_pages(pages, PAGES) }.map_err(|error| error.status())?;
        self.root.store(translation_root(), Ordering::Relaxed);
        self.start.store(pages.as_ptr().addr(), Ordering::Release);
        Ok(())
    }

    pub(super) fn inspect(&self, boot: &Boot<'_>) -> PhysicalRange {
        let start = self.start.load(Ordering::Acquire);
        assert_ne!(start, 0);
        self.check_root();
        let end = start
            .checked_add(PAGES.checked_mul(PAGE_SIZE).unwrap())
            .unwrap();
        let scope = extent(start as u64, end as u64);
        let map = boot.firmware_map().expect("final UEFI memory map");
        assert!(map.regions().any(|region| {
            region.kind == MemoryType::CONVENTIONAL.0 && region.physical.covers(scope)
        }));
        assert!(
            boot.exclusions()
                .entries()
                .all(|entry| !entry.physical.overlaps(scope))
        );
        for page in (scope.start().get()..scope.end().get()).step_by(PAGE_SIZE) {
            assert_writable_identity(page);
        }
        scope
    }

    pub(super) fn check_root(&self) {
        assert_eq!(translation_root(), self.root.load(Ordering::Relaxed));
    }
}

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

#[cfg(target_arch = "aarch64")]
fn assert_writable_identity(page: u64) {
    const OUTPUT: u64 = 0x0000_ffff_ffff_f000;
    let native: u64;
    unsafe {
        asm!(
            "at s1e1w, {page}", "isb", "mrs {native}, par_el1",
            page = in(reg) page, native = out(reg) native, options(nostack),
        );
    }
    assert_eq!(native & (OUTPUT | 1), page);
}

#[cfg(target_arch = "x86_64")]
fn assert_writable_identity(page: u64) {
    use kernel::metal::memory::mmu::x86_64::internal::context::Controls;

    const ADDRESS: u64 = 0x000f_ffff_ffff_f000;
    const PRESENT_AND_WRITABLE: u64 = 0b11;
    const LARGE: u64 = 1 << 7;

    let controls = unsafe { Controls::current() };
    assert!(controls.four_level_untagged());
    assert!(page < 1 << 47);
    let mut table = controls.cr3 as u64 & ADDRESS;
    for shift in [39, 30, 21, 12] {
        assert_ne!(table, 0);
        let index = ((page >> shift) & 0x1ff) as usize;
        // OVMF keeps the original resident page tables identity-mapped in
        // this fixture. Follow only the active hierarchy for this RAM page.
        let words = core::ptr::with_exposed_provenance::<u64>(table as usize);
        let entry = unsafe { words.wrapping_add(index).read_volatile() };
        assert_eq!(entry & PRESENT_AND_WRITABLE, PRESENT_AND_WRITABLE);
        if shift == 12 || ((shift == 30 || shift == 21) && entry & LARGE != 0) {
            let offset = (1 << shift) - 1;
            assert_eq!((entry & ADDRESS & !offset) | (page & offset), page);
            return;
        }
        assert_eq!(entry & LARGE, 0);
        table = entry & ADDRESS;
    }
}
