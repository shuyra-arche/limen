use core::ptr::{self, NonNull};

use kernel::access::MappedRange;
use kernel::address::{PhysicalAddress, PhysicalRange};
#[cfg(target_arch = "riscv64")]
use kernel::entry::boot::{Boot, Condition, Retirement};

pub(super) fn extent(start: u64, end: u64) -> PhysicalRange {
    PhysicalRange::new(PhysicalAddress::new(start), PhysicalAddress::new(end)).unwrap()
}

// The boot guest reserves this test-only RAM window in the bare virt fixture.
// It is outside the image, both startup stacks, OpenSBI and the retained DTB.
// No minor or DMA-capable guest driver has run and satp is still zero. The
// resident stack is permanent for this guest and stays excluded.
#[cfg(target_arch = "riscv64")]
pub(super) fn retirement(boot: &Boot<'_>, physical: PhysicalRange) -> Retirement {
    let exclusions = boot.exclusions();
    assert!(
        exclusions
            .entries()
            .all(|entry| !entry.physical.overlaps(physical))
    );
    assert!(
        boot.dtb_memory()
            .unwrap()
            .ram()
            .any(|ram| ram.covers(physical))
    );

    let mut retirement = Retirement::new(physical);
    for condition in [
        Condition::RuntimeStack,
        Condition::ReadersGone,
        Condition::TranslationRetired,
        Condition::DevicesIsolated,
    ] {
        unsafe { retirement.record(condition) };
    }
    retirement
}

/// The caller owns the entire identity-mapped extent and keeps all
/// other software and devices excluded for the returned borrow's lifetime.
pub(super) unsafe fn mapped(physical: PhysicalRange) -> MappedRange<'static> {
    // This guest-owned machine RAM is outside any Rust allocation. The
    // platform admits it with a live writable identity mapping.
    let start = usize::try_from(physical.start().get()).unwrap();
    let base = NonNull::new(ptr::with_exposed_provenance_mut(start)).unwrap();
    unsafe { MappedRange::from_identity(physical, base) }.unwrap()
}
