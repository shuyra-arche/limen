use kernel::{
    entry::boot::{Boot, Condition, Retirement},
    metal::memory::allocator::treaty::RegionOrigin,
};
use limen_treaty::model::ByteLength;
use uefi::{Status, boot::PAGE_SIZE};

use super::{foundation, window::Window};

static WINDOW: Window<{ foundation::PAGES }> = Window::new();

pub(super) fn prepare() -> Result<(), Status> {
    WINDOW.prepare()
}

pub(super) fn check(boot: &Boot<'_>) {
    let scope = WINDOW.inspect(boot);
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
}
