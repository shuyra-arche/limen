use kernel::{
    entry::boot::{Boot, Condition, Retirement},
    metal::memory::allocator::treaty::RegionOrigin,
};
use limen_treaty::model::ByteLength;
use uefi::{Status, boot::PAGE_SIZE};

use super::{foundation, window::Window};

#[cfg(target_arch = "aarch64")]
#[path = "../../../metal/memory/mmu/aarch64/owned.rs"]
mod upper;

#[cfg(target_arch = "aarch64")]
const UPPER_PAGES: usize = upper::PAGES;
#[cfg(target_arch = "x86_64")]
const UPPER_PAGES: usize = 0;
const PAGES: usize = foundation::PAGES + UPPER_PAGES;
static WINDOW: Window<PAGES> = Window::new();

pub(super) fn prepare() -> Result<(), Status> {
    WINDOW.prepare()
}

pub(super) fn check(boot: &Boot<'_>) {
    let window = WINDOW.inspect(boot);
    let scope = window
        .start()
        .range(ByteLength::new((foundation::PAGES * PAGE_SIZE) as u64))
        .unwrap();
    assert!(window.covers(scope));
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
        let range = limen_treaty::model::PhysicalRange::new(scope.end(), window.end()).unwrap();
        let mut evidence = Retirement::new(range);
        // The original allocator never admitted these independently
        // inspected pages of the same private conventional RAM extent.
        unsafe {
            evidence.record(Condition::NoEarlyBorrow);
            evidence.record(Condition::TranslationRetired);
            evidence.record(Condition::DevicesIsolated);
            upper::check(boot, range, &evidence);
        }
    }
}
