use core::{mem::MaybeUninit, ptr, slice};

use kernel::entry::boot::{Boot, Condition, Physical, Retirement};

// The boot guest reserves this test-only RAM window in the bare virt fixture.
// It is outside the image, both startup stacks, OpenSBI and the retained DTB.
// No minor or DMA-capable guest driver has run and satp is still zero. The
// resident stack is permanent for this guest and stays excluded.
pub(super) fn retirement(boot: &Boot<'_>, physical: Physical) -> Retirement {
    let exclusions = boot.exclusions();
    assert!(exclusions.entries().all(|entry| {
        entry.physical.end <= physical.start || entry.physical.start >= physical.end
    }));
    assert!(
        boot.dtb_memory()
            .unwrap()
            .ram()
            .iter()
            .any(|ram| { ram.start <= physical.start && physical.end <= ram.end })
    );

    let mut retirement = Retirement::new(physical).unwrap();
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

/// The caller owns the entire bare, identity-mapped extent and keeps all
/// other software and devices excluded for the returned slice's lifetime.
pub(super) unsafe fn mapped(physical: Physical) -> &'static mut [MaybeUninit<u8>] {
    let base = ptr::with_exposed_provenance_mut(physical.start as usize);
    unsafe { slice::from_raw_parts_mut(base, (physical.end - physical.start) as usize) }
}
